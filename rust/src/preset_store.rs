//! Presets of the user: named sets of option fields, one JSON file for each kind in the settings directory of
//! the host (`{"presets": {name: entry}}`). They belong to the user of the machine and to no document, so saving
//! one is no undo step. Each kind has a file of its own, and none is a key of `settings.json`, which the editor
//! holds in memory and writes whole.

use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_PRESETS: usize = 64;
const MAX_NAME: usize = 64;
const MAX_FILE: u64 = 1 << 20;

fn store_directory(dir: &Path) -> Result<PathBuf, String> {
    if dir.as_os_str().is_empty() { return Err("settings directory must not be empty".into()); }
    crate::export_ops::resolve_collection_path(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("soul2d-store-plan-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        crate::test_scratch::scratch(root)
    }

    fn kind() -> Kind {
        Kind { file: "presets.json", noun: "preset", entry: |v| Ok(json!({"value":v["value"]})), builtin: |_| false }
    }

    #[test]
    fn store_preparation_creates_nothing_and_execute_preserves_the_response() {
        let root = root();let dir = root.join("missing/nested");let kind = kind();
        let plan = kind.prepare_save(&dir, &json!({"name":"A","value":1})).unwrap();
        assert!(!root.join("missing").exists());
        assert_eq!(plan.execute(Some(&dir)).unwrap(), json!({"name":"A","preset":{"value":1},"replaced":false}));
        assert_eq!(kind.load(&dir).unwrap(), json!({"A":{"value":1}}).as_object().unwrap().clone());
        let plan = kind.prepare_delete(&dir, &json!({"name":"A"})).unwrap();
        assert_eq!(plan.execute(Some(&dir)).unwrap(), json!({"name":"A"}));
        assert!(kind.load(&dir).unwrap().is_empty());
        assert!(kind.prepare_save(Path::new(""), &json!({"name":"A"})).is_err());
    }

    #[test]
    fn store_changes_and_config_switches_are_refused_before_writing() {
        for change in ["other_entry", "format", "removed", "config", "appeared"] {
            let dir = root();let kind = kind();let file = dir.join(kind.file);
            if change != "appeared" {
                kind.prepare_save(&dir, &json!({"name":"A","value":1})).unwrap().execute(Some(&dir)).unwrap();
            }
            let plan = kind.prepare_save(&dir, &json!({"name":"B","value":2})).unwrap();
            match change {
                "other_entry" | "appeared" => fs::write(&file, br#"{"presets":{"Other":{"value":3}}}"#).unwrap(),
                "format" => fs::write(&file, br#"{ "presets": {"A": {"value":1}} }"#).unwrap(),
                "removed" => fs::remove_file(&file).unwrap(),
                _ => {},
            }
            let before = fs::read(&file).ok();let other = dir.join("different");
            let configured = if change == "config" { &other } else { &dir };
            assert!(plan.execute(Some(configured)).is_err(), "{change}");
            assert_eq!(fs::read(&file).ok(), before);assert!(!other.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn store_leaf_links_are_replaced_without_writing_their_referents() {
        use std::os::unix::fs::symlink;
        let dir = root();let kind = kind();let file = dir.join(kind.file);
        let first = dir.join("first.json");let second = dir.join("second.json");
        let original = br#"{"presets":{"A":{"value":1}}}"#;
        fs::write(&first, original).unwrap();fs::write(&second, original).unwrap();symlink(&first, &file).unwrap();
        let plan = kind.prepare_save(&dir, &json!({"name":"B","value":2})).unwrap();
        plan.execute(Some(&dir)).unwrap();
        assert!(!fs::symlink_metadata(&file).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&first).unwrap(), original);assert_eq!(kind.load(&dir).unwrap().len(), 2);
        fs::remove_file(&file).unwrap();symlink(&first, &file).unwrap();
        let plan = kind.prepare_save(&dir, &json!({"name":"B","value":2})).unwrap();
        fs::remove_file(&file).unwrap();symlink(&second, &file).unwrap();
        assert!(plan.execute(Some(&dir)).is_err());assert_eq!(fs::read(&second).unwrap(), original);
        fs::remove_file(&file).unwrap();symlink(dir.join("absent.json"), &file).unwrap();
        kind.prepare_save(&dir, &json!({"name":"B","value":2})).unwrap().execute(Some(&dir)).unwrap();
        assert!(!dir.join("absent.json").exists());assert_eq!(kind.load(&dir).unwrap().len(), 1);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct StoredFile {
    bytes: Option<Arc<[u8]>>,
    link: Option<PathBuf>,
    resolved: Option<PathBuf>,
}

fn read_file(path: &Path) -> Result<StoredFile, String> {
    let binding = || (fs::read_link(path).ok(), fs::canonicalize(path).ok());
    let (link, resolved) = binding();
    let bytes = match fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
            Some(bytes.into())
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if binding() != (link.clone(), resolved.clone()) { return Err("store path changed while reading".into()); }
    Ok(StoredFile { bytes, link, resolved })
}

/// One complete preset-store replacement, prepared without creating directories or files.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreWritePlan {
    pub target: PathBuf,
    pub name: String,
    pub deleting: bool,
    pub replaced: bool,
    directory_alias: PathBuf,
    directory: PathBuf,
    original: StoredFile,
    bytes: Arc<[u8]>,
    pub(crate) response: Value,
}

impl StoreWritePlan {
    fn validate(&self, configured: Option<&Path>) -> Result<(), String> {
        if configured != Some(self.directory_alias.as_path())
            || store_directory(&self.directory_alias)? != self.directory {
            return Err("preset store directory changed after preparation".into());
        }
        if read_file(&self.target)? != self.original { return Err("preset store changed after preparation".into()); }
        Ok(())
    }

    pub(crate) fn execute(self, configured: Option<&Path>) -> Result<Value, String> {
        self.validate(configured)?;
        fs::create_dir_all(&self.directory).map_err(|error| format!("cannot create the settings directory: {error}"))?;
        self.validate(configured)?;
        // shortcut: another process can write after validation; add locking if concurrent writers must be serialized.
        crate::write_atomic(&self.target, &self.bytes, true)?;
        Ok(self.response)
    }
}

/// One kind of preset.
pub(crate) struct Kind {
    /// Its file in the settings directory.
    pub file: &'static str,
    /// What its messages call one entry, such as "preset".
    pub noun: &'static str,
    /// What a preset keeps of a request, checked. Every entry read from the file goes through it as well.
    pub entry: fn(&Value) -> Result<Value, String>,
    /// Whether the kind has a preset of this name built in. Such a name cannot be saved.
    pub builtin: fn(&str) -> bool,
}

impl Kind {
    fn name<'a>(&self, request: &'a Value) -> Result<&'a str, String> {
        let name = request.get("name").and_then(Value::as_str).ok_or("name must be a string")?;
        let length = name.chars().count();
        if length == 0 || length > MAX_NAME || name != name.trim() || name.chars().any(char::is_control) {
            return Err(format!(
                "name must be 1 to {MAX_NAME} characters, with no control character and no white space at either end"
            ));
        }
        if (self.builtin)(name) {
            return Err(format!("{name:?} is a built-in {}", self.noun));
        }
        Ok(name)
    }

    /// The presets of the settings directory `dir`, by name. A directory without the file has none.
    pub(crate) fn load(&self, dir: &Path) -> Result<Map<String, Value>, String> {
        self.load_snapshot(dir).map(|(presets, _)| presets)
    }

    fn load_snapshot(&self, dir: &Path) -> Result<(Map<String, Value>, StoredFile), String> {
        let (file, noun) = (self.file, self.noun);
        let snapshot = read_file(&dir.join(file))
            .map_err(|error| format!("cannot read {file} of the settings directory: {error}"))?;
        let Some(bytes) = &snapshot.bytes else { return Ok((Map::new(), snapshot)) };
        let invalid = |reason: String| format!("{file} of the settings directory is not valid: {reason}");
        if bytes.len() as u64 > MAX_FILE {
            return Err(invalid(format!("it is larger than {MAX_FILE} bytes")));
        }
        let parsed: Value = serde_json::from_slice(&bytes).map_err(|error| invalid(error.to_string()))?;
        let stored = parsed
            .get("presets")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("presets must be an object".into()))?;
        if stored.len() > MAX_PRESETS {
            return Err(invalid(format!("it holds more than {MAX_PRESETS} {noun}s")));
        }
        let mut presets = Map::new();
        for (name, entry) in stored {
            self.name(&json!({ "name": name })).map_err(&invalid)?;
            let entry = (self.entry)(entry).map_err(|error| invalid(format!("{noun} {name:?}: {error}")))?;
            presets.insert(name.clone(), entry);
        }
        Ok((presets, snapshot))
    }

    fn plan(&self, dir: &Path, directory: PathBuf, original: StoredFile, presets: &Map<String, Value>, response: Value) -> Result<StoreWritePlan, String> {
        let bytes = serde_json::to_vec_pretty(&json!({ "presets": presets })).map_err(|error| error.to_string())?;
        // A number keeps the digits it was written with, so a few fields can be longer than the file may be.
        if bytes.len() as u64 > MAX_FILE {
            return Err(format!("the {}s would be larger than {MAX_FILE} bytes", self.noun));
        }
        Ok(StoreWritePlan {
            target: directory.join(self.file), name: response["name"].as_str().unwrap().to_owned(),
            deleting: response.get("preset").is_none(), replaced: response["replaced"].as_bool().unwrap_or(false),
            directory_alias: dir.to_owned(), directory, original, bytes: bytes.into(), response,
        })
    }

    /// Keeps the entry of `request` under its `name`. A preset of that name is replaced only with `overwrite`,
    /// like a file an export would replace: there is no undo step to bring it back.
    pub(crate) fn prepare_save(&self, dir: &Path, request: &Value) -> Result<StoreWritePlan, String> {
        let name = self.name(request)?;
        let entry = (self.entry)(request)?;
        let overwrite = match request.get("overwrite") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(overwrite)) => *overwrite,
            Some(_) => return Err("overwrite must be a boolean".into()),
        };
        let directory = store_directory(dir)?;
        let (mut presets, original) = self.load_snapshot(&directory)?;
        let replaced = presets.insert(name.to_owned(), entry.clone()).is_some();
        if replaced && !overwrite {
            return Err(format!("a {} named {name:?} is saved; pass overwrite to replace it", self.noun));
        }
        if presets.len() > MAX_PRESETS {
            return Err(format!("at most {MAX_PRESETS} {}s can be saved", self.noun));
        }
        self.plan(dir, directory, original, &presets, json!({ "name": name, "preset": entry, "replaced": replaced }))
    }

    /// Deletes the preset `name`. A name that is not saved is refused, so a misspelt name is not taken for done.
    pub(crate) fn prepare_delete(&self, dir: &Path, request: &Value) -> Result<StoreWritePlan, String> {
        let name = self.name(request)?;
        let directory = store_directory(dir)?;
        let (mut presets, original) = self.load_snapshot(&directory)?;
        if presets.remove(name).is_none() {
            return Err(format!("no saved {} is named {name:?}", self.noun));
        }
        self.plan(dir, directory, original, &presets, json!({ "name": name }))
    }
}
