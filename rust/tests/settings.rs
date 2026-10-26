use soul2d_core::settings::{
    ensure_fonts_dir, ensure_locale_dir, imgui_ini_path, locale_dir_extra, resolve_config_dir, settings_path,
    Chord, Platform, Settings, ShortcutRegistry, DEFAULT_SHORTCUTS,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "soul2d-settings-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())
}

fn resolve(platform: Platform, pairs: &[(&str, &str)], legacy_exists: bool) -> PathBuf {
    resolve_config_dir(platform, &env_of(pairs), Path::new("/work"), &|path| {
        legacy_exists && path.file_name().is_some_and(|name| name == ".soul2d")
    })
}

// ---- config paths ----

#[test]
fn forced_config_dir_wins_on_every_platform() {
    for platform in [Platform::Windows, Platform::MacOs, Platform::Linux, Platform::OtherPosix] {
        let env = [("INOCHI_CONFIG_PATH", "/forced/cfg"), ("HOME", "/home/u"), ("AppData", "C:/ad")];
        assert_eq!(resolve(platform, &env, true), PathBuf::from("/forced/cfg"));
    }
    // An empty override counts as unset.
    assert_eq!(
        resolve(Platform::Linux, &[("INOCHI_CONFIG_PATH", ""), ("HOME", "/home/u")], false),
        PathBuf::from("/home/u/.config/soul2d")
    );
}

#[test]
fn linux_uses_xdg_then_home_config_and_keeps_legacy_dir() {
    assert_eq!(
        resolve(Platform::Linux, &[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")], false),
        PathBuf::from("/xdg/soul2d")
    );
    assert_eq!(resolve(Platform::Linux, &[("HOME", "/home/u")], false), PathBuf::from("/home/u/.config/soul2d"));
    assert_eq!(resolve(Platform::Linux, &[("HOME", "/home/u")], true), PathBuf::from("/home/u/.config/.soul2d"));
    // D joins an unset HOME to an empty path, which leaves a relative ".config".
    assert_eq!(resolve(Platform::Linux, &[], false), PathBuf::from(".config/soul2d"));
}

#[test]
fn macos_uses_application_support_and_keeps_legacy_dir() {
    assert_eq!(
        resolve(Platform::MacOs, &[("HOME", "/Users/u")], false),
        PathBuf::from("/Users/u/Library/Application Support/soul2d")
    );
    assert_eq!(
        resolve(Platform::MacOs, &[("HOME", "/Users/u")], true),
        PathBuf::from("/Users/u/Library/Application Support/.soul2d")
    );
    assert_eq!(resolve(Platform::MacOs, &[], false), PathBuf::from("/work/soul2d"));
}

#[test]
fn windows_and_other_posix_use_dot_folder() {
    assert_eq!(
        resolve(Platform::Windows, &[("AppData", "C:/Users/u/AppData/Roaming")], true),
        PathBuf::from("C:/Users/u/AppData/Roaming/.soul2d")
    );
    assert_eq!(resolve(Platform::Windows, &[], false), PathBuf::from("/work/.soul2d"));
    assert_eq!(resolve(Platform::OtherPosix, &[("HOME", "/home/u")], false), PathBuf::from("/home/u/.soul2d"));
    assert_eq!(resolve(Platform::OtherPosix, &[], false), PathBuf::from("/work/.soul2d"));
}

#[test]
fn derived_paths_and_created_subdirectories() {
    let temp = TempDir::new("paths");
    let config = temp.path().join("cfg");
    assert_eq!(settings_path(&config), config.join("settings.json"));
    assert_eq!(imgui_ini_path(&config), config.join("imgui.ini"));
    let fonts = ensure_fonts_dir(&config).unwrap();
    let locale = ensure_locale_dir(&config).unwrap();
    assert_eq!(fonts, config.join("fonts"));
    assert_eq!(locale, config.join("i18n"));
    assert!(fonts.is_dir() && locale.is_dir());

    let here = env_of(&[("HERE", "/tmp/.mount_app")]);
    assert_eq!(locale_dir_extra(Platform::Linux, &here), Some(PathBuf::from("/tmp/.mount_app")));
    assert_eq!(locale_dir_extra(Platform::MacOs, &here), None);
    assert_eq!(locale_dir_extra(Platform::Linux, &env_of(&[])), None);
}

// ---- settings store ----

// Compact output as D's JSONValue.toString() writes it (floats with no fraction lose the ".0").
const D_SAMPLE: &str = r#"{"ViewportZoomMode":"ScreenCenter","ViewportZoomSpeed":5,"UiScale":1.25,"MaxUndoHistory":42,"WinMax":false,"RecentFiles":["a.inx","b.inx"],"MCP.Servers":{"local":"127.0.0.1:39200"},"Shortcuts":{"NodeCommand.AddNode":"Ctrl+Alt+N"},"Future.Key":{"nested":[1,2,{"a":null}]}}"#;

#[test]
fn loads_a_d_written_file_and_reads_typed_values() {
    let temp = TempDir::new("dsample");
    let path = settings_path(temp.path());
    fs::write(&path, D_SAMPLE).unwrap();
    let loaded = Settings::load(&path).unwrap();
    assert!(loaded.corrupted.is_none());
    let settings = loaded.settings;
    // An existing file does not get the new-user defaults: it keeps its own zoom mode.
    assert_eq!(settings.get::<String>("ViewportZoomMode").unwrap(), "ScreenCenter");
    assert_eq!(settings.get::<f64>("ViewportZoomSpeed").unwrap(), 5.0);
    assert_eq!(settings.get::<f64>("UiScale").unwrap(), 1.25);
    assert_eq!(settings.get::<u64>("MaxUndoHistory").unwrap(), 42);
    assert_eq!(settings.get::<i64>("MaxUndoHistory").unwrap(), 42);
    assert!(!settings.get::<bool>("WinMax").unwrap());
    assert_eq!(settings.get::<Vec<String>>("RecentFiles").unwrap(), vec!["a.inx", "b.inx"]);
    let servers: BTreeMap<String, String> = settings.get("MCP.Servers").unwrap();
    assert_eq!(servers["local"], "127.0.0.1:39200");
    assert!(settings.contains("Future.Key"));
    assert!(!settings.contains("KeepLayerFolder"));
}

#[test]
fn missing_values_use_defaults_and_wrong_types_are_errors() {
    let settings = Settings::parse(D_SAMPLE.as_bytes()).unwrap();
    assert_eq!(settings.get::<i64>("WinW").unwrap(), 0);
    assert_eq!(settings.get_or::<i64>("WinW", 1280).unwrap(), 1280);
    assert_eq!(settings.get_or::<String>("lang", "en".to_string()).unwrap(), "en");
    assert_eq!(settings.get::<Vec<String>>("prev_projects").unwrap(), Vec::<String>::new());
    assert!(settings.get::<bool>("ViewportZoomMode").unwrap_err().contains("ViewportZoomMode"));
    assert!(settings.get::<i64>("UiScale").is_err());
    assert!(settings.get::<Vec<String>>("MaxUndoHistory").is_err());
    assert!(settings.get::<BTreeMap<String, String>>("RecentFiles").is_err());
    let mut mixed = Settings::parse(br#"{"List":["a",1]}"#).unwrap();
    assert!(mixed.get::<Vec<String>>("List").is_err());
    assert!(mixed.set("Speed", f64::NAN).is_err());
    assert!(!mixed.contains("Speed"));
}

#[test]
fn set_save_load_round_trip_keeps_unknown_keys() {
    let temp = TempDir::new("roundtrip");
    let path = settings_path(&temp.path().join("nested"));
    let mut settings = Settings::parse(D_SAMPLE.as_bytes()).unwrap();
    settings.set("Language", "ja_JP".to_string()).unwrap();
    settings.set("UiScale", 1.5f64).unwrap();
    settings.set("ShowDebugOverlay", true).unwrap();
    settings.set("WinW", 1920i64).unwrap();
    settings.set("RecentFiles", vec!["c.inx".to_string()]).unwrap();
    settings.save(&path).unwrap();
    assert!(!temp.path().join("nested").join("settings.json.swp").exists());

    let reloaded = Settings::load(&path).unwrap().settings;
    assert_eq!(reloaded, settings);
    assert_eq!(reloaded.get::<String>("Language").unwrap(), "ja_JP");
    assert_eq!(reloaded.get::<f64>("UiScale").unwrap(), 1.5);
    assert!(reloaded.get::<bool>("ShowDebugOverlay").unwrap());
    assert_eq!(reloaded.get::<i64>("WinW").unwrap(), 1920);
    assert_eq!(reloaded.get::<Vec<String>>("RecentFiles").unwrap(), vec!["c.inx"]);
    assert_eq!(reloaded.get::<String>("ViewportZoomMode").unwrap(), "ScreenCenter");
    let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk["Future.Key"], serde_json::json!({"nested": [1, 2, {"a": null}]}));
    assert_eq!(on_disk["Shortcuts"]["NodeCommand.AddNode"], "Ctrl+Alt+N");
}

#[test]
fn missing_file_gives_new_user_defaults() {
    let temp = TempDir::new("missing");
    let loaded = Settings::load(&settings_path(temp.path())).unwrap();
    assert!(loaded.corrupted.is_none());
    let settings = loaded.settings;
    assert_eq!(settings.get::<String>("ViewportZoomMode").unwrap(), "MousePosition");
    assert_eq!(settings.get::<f64>("ViewportZoomSpeed").unwrap(), 5.0);
    assert_eq!(settings.get::<String>("KeepLayerFolder").unwrap(), "Ask");
    assert_eq!(settings.get::<String>("LayerGroupReplacement").unwrap(), "Node");
    assert!(!settings.contains("Shortcuts"));
}

#[test]
fn corrupt_files_are_moved_aside_byte_for_byte() {
    let cases: [(&str, &[u8]); 5] = [
        ("truncated", br#"{"ViewportZoomMode":"Mouse"#),
        ("empty-object", b"{}"),
        ("array-root", b"[1,2,3]"),
        ("zero-bytes", b""),
        ("invalid-utf8", b"{\"a\":\"\xff\xfe\"}"),
    ];
    for (label, bytes) in cases {
        let temp = TempDir::new(label);
        let path = settings_path(temp.path());
        fs::write(&path, bytes).unwrap();
        let loaded = Settings::load(&path).unwrap();
        let corrupted = loaded.corrupted.unwrap_or_else(|| panic!("{label} should be reported corrupt"));
        assert!(!corrupted.reason.is_empty(), "{label}");
        assert!(!path.exists(), "{label}: corrupt file must be moved away");
        assert_eq!(fs::read(&corrupted.backup).unwrap(), bytes, "{label}: backup keeps the original bytes");
        assert_eq!(corrupted.backup.parent(), path.parent());
        let name = corrupted.backup.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.starts_with("settings.json.20") && name.ends_with('Z'), "{label}: {name}");
        assert_eq!(loaded.settings.get::<String>("ViewportZoomMode").unwrap(), "MousePosition", "{label}");
    }
}

#[test]
fn unreadable_settings_path_is_an_error_not_corruption() {
    let temp = TempDir::new("dirpath");
    let path = settings_path(temp.path());
    fs::create_dir(&path).unwrap();
    assert!(Settings::load(&path).is_err());
    assert!(path.is_dir());
}

// ---- chords ----

#[test]
fn spec_parsing_covers_every_modifier_and_key_kind() {
    let chord = Chord::from_spec("Ctrl-N").unwrap();
    assert_eq!((chord.ctrl, chord.alt, chord.shift, chord.super_key, chord.key.as_str()), (true, false, false, false, "N"));
    let chord = Chord::from_spec("Alt-F12").unwrap();
    assert_eq!((chord.ctrl, chord.alt, chord.shift, chord.super_key, chord.key.as_str()), (false, true, false, false, "F12"));
    let chord = Chord::from_spec("Shift-Left").unwrap();
    assert_eq!((chord.ctrl, chord.alt, chord.shift, chord.super_key, chord.key.as_str()), (false, false, true, false, "Left"));
    let chord = Chord::from_spec("Super-0").unwrap();
    assert_eq!((chord.ctrl, chord.alt, chord.shift, chord.super_key, chord.key.as_str()), (false, false, false, true, "0"));
    let chord = Chord::from_spec("Super-Shift-Alt-Ctrl-Z").unwrap();
    assert_eq!((chord.ctrl, chord.alt, chord.shift, chord.super_key, chord.key.as_str()), (true, true, true, true, "Z"));
    let bare = Chord::from_spec("F1").unwrap();
    assert_eq!((bare.ctrl, bare.alt, bare.shift, bare.super_key, bare.key.as_str()), (false, false, false, false, "F1"));
    for key in ["Right", "Up", "Down", "A", "9", "F9", "F10", "F11"] {
        assert_eq!(Chord::from_spec(&format!("Ctrl-{key}")).unwrap().key, key);
    }
}

#[test]
fn formatting_uses_d_order_and_platform_labels() {
    let all = Chord::from_spec("Super-Shift-Alt-Ctrl-Z").unwrap();
    assert_eq!(all.format(Platform::Linux), "Ctrl+Alt+Shift+Super+Z");
    assert_eq!(all.format(Platform::Windows), "Ctrl+Alt+Shift+Super+Z");
    assert_eq!(all.format(Platform::MacOs), "\u{eae6}+\u{eae8}+Shift+\u{eae7}+Z");
    assert_eq!(Chord::from_spec("Ctrl-Shift-S").unwrap().format(Platform::Linux), "Ctrl+Shift+S");
    assert_eq!(Chord::from_spec("Ctrl-Shift-S").unwrap().format(Platform::MacOs), "\u{eae6}+Shift+S");
    assert_eq!(Chord::from_spec("F5").unwrap().format(Platform::Linux), "F5");
    assert_eq!(Chord::from_spec("Alt-Down").unwrap().format(Platform::MacOs), "\u{eae8}+Down");
}

#[test]
fn stored_text_parses_back_per_platform() {
    for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
        let chord = Chord::from_spec("Ctrl-Alt-Shift-Super-F7").unwrap();
        assert_eq!(Chord::parse(&chord.format(platform), platform).unwrap(), chord);
    }
    let chord = Chord::parse("Ctrl+Alt+N", Platform::Linux).unwrap();
    assert_eq!(chord, Chord::from_spec("Ctrl-Alt-N").unwrap());
    // Modifier order in stored text does not matter for matching.
    assert_eq!(Chord::parse("Shift+Ctrl+S", Platform::Linux).unwrap(), Chord::from_spec("Ctrl-Shift-S").unwrap());
    // The text label is not a macOS label and the glyph is not a Linux label.
    assert!(Chord::parse("Ctrl+N", Platform::MacOs).is_err());
    assert!(Chord::parse("\u{eae6}+N", Platform::Linux).is_err());
}

#[test]
fn invalid_chord_text_is_rejected() {
    for spec in ["", "Ctrl-", "-N", "Ctrl", "Shift", "Ctrl-Hyper-N", "Ctrl-n", "Ctrl-F13", "Ctrl-F0", "Ctrl-Space", "ctrl-N", "Ctrl-AB"] {
        assert!(Chord::from_spec(spec).is_err(), "spec {spec:?} must be rejected");
    }
    for text in ["", "Ctrl+", "+N", "Ctrl+Ctrl2+N", "Ctrl+Plus", "Ctrl+N+", "Ctrl-N", "Meta+N"] {
        assert!(Chord::parse(text, Platform::Linux).is_err(), "text {text:?} must be rejected");
    }
    assert!(Chord::from_spec("Ctrl-Hyper-N").unwrap_err().contains("Hyper"));
}

#[test]
fn mac_swap_exchanges_ctrl_and_super_only() {
    let chord = Chord::from_spec("Ctrl-Shift-S").unwrap();
    let swapped = chord.swap_ctrl_super();
    assert_eq!(swapped, Chord::from_spec("Super-Shift-S").unwrap());
    assert_eq!(swapped.swap_ctrl_super(), chord);
    let alt = Chord::from_spec("Alt-Up").unwrap();
    assert_eq!(alt.swap_ctrl_super(), alt);
}

// ---- registry ----

#[test]
fn default_bindings_match_defaults_d() {
    let registry = ShortcutRegistry::with_defaults();
    let expected = [
        ("FileCommand.NewFile", "Ctrl+N", false),
        ("FileCommand.ShowOpenFileDialog", "Ctrl+O", false),
        ("FileCommand.ShowSaveFileDialog", "Ctrl+S", false),
        ("FileCommand.ShowSaveFileAsDialog", "Ctrl+Shift+S", false),
        ("EditCommand.Redo", "Ctrl+Shift+Z", true),
        ("EditCommand.Undo", "Ctrl+Z", true),
        ("NodeCommand.CutNode", "Ctrl+X", true),
        ("NodeCommand.CopyNode", "Ctrl+C", true),
        ("NodeCommand.PasteNode", "Ctrl+V", true),
        ("PaletteCommand.ListCommand", "Ctrl+Shift+P", false),
        // Soul2D's own: D bound no key to these two.
        ("AnimationCommand.KeyAll", "Ctrl+K", false),
        ("AnimationCommand.UnkeyAll", "Ctrl+Shift+K", false),
    ];
    assert_eq!(DEFAULT_SHORTCUTS.len(), expected.len());
    assert_eq!(registry.iter().count(), expected.len());
    for (id, text, repeat) in expected {
        let binding = registry.binding_for(id).unwrap_or_else(|| panic!("{id} unbound"));
        assert_eq!(binding.chord.format(Platform::Linux), text, "{id}");
        assert_eq!(binding.repeat, repeat, "{id}");
    }
    let undo = Chord::from_spec("Ctrl-Z").unwrap();
    assert_eq!(registry.command_for(&undo), Some("EditCommand.Undo"));
}

#[test]
fn registering_a_taken_chord_displaces_the_old_command() {
    let mut registry = ShortcutRegistry::new();
    let chord = Chord::from_spec("Ctrl-Alt-N").unwrap();
    assert_eq!(registry.register("NodeCommand.AddNode", chord.clone(), false), None);
    assert_eq!(registry.binding_for("NodeCommand.AddNode").unwrap().chord, chord);

    let displaced = registry.register("NodeCommand.DeleteNode", chord.clone(), false);
    assert_eq!(displaced.as_deref(), Some("NodeCommand.AddNode"));
    assert!(registry.binding_for("NodeCommand.AddNode").is_none());
    assert_eq!(registry.command_for(&chord), Some("NodeCommand.DeleteNode"));
    assert_eq!(registry.iter().count(), 1);

    // Re-registering the same command with the same chord displaces nobody.
    assert_eq!(registry.register("NodeCommand.DeleteNode", chord.clone(), true), None);
    assert!(registry.binding_for("NodeCommand.DeleteNode").unwrap().repeat);
}

#[test]
fn rebinding_a_command_drops_its_previous_chord() {
    let mut registry = ShortcutRegistry::with_defaults();
    registry.register("EditCommand.Undo", Chord::from_spec("Ctrl-Alt-Z").unwrap(), false);
    assert!(registry.command_for(&Chord::from_spec("Ctrl-Z").unwrap()).is_none());
    assert_eq!(registry.iter().count(), 12);
    assert!(registry.clear("EditCommand.Undo"));
    assert!(!registry.clear("EditCommand.Undo"));
    assert_eq!(registry.iter().count(), 11);
}

#[test]
fn save_and_load_round_trip_through_settings_file() {
    let temp = TempDir::new("shortcuts");
    let path = settings_path(temp.path());
    let mut registry = ShortcutRegistry::new();
    registry.register("NodeCommand.AddNode", Chord::from_spec("Ctrl-Alt-N").unwrap(), false);
    registry.register("NodeCommand.DeleteNode", Chord::from_spec("Ctrl-Alt-N").unwrap(), false);
    let mut settings = Settings::parse(br#"{"Keep":1}"#).unwrap();
    registry.save_to(&mut settings, Platform::Linux).unwrap();
    settings.save(&path).unwrap();

    let on_disk: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(on_disk["Shortcuts"], serde_json::json!({"NodeCommand.DeleteNode": "Ctrl+Alt+N"}));
    assert_eq!(on_disk["Keep"], 1);

    let reloaded = Settings::load(&path).unwrap().settings;
    let mut restored = ShortcutRegistry::new();
    assert_eq!(restored.load_from(&reloaded, Platform::Linux).unwrap(), Vec::<String>::new());
    assert_eq!(restored.command_for(&Chord::from_spec("Ctrl-Alt-N").unwrap()), Some("NodeCommand.DeleteNode"));
    assert!(restored.binding_for("NodeCommand.AddNode").is_none());
}

#[test]
fn macos_overrides_are_stored_with_glyph_labels() {
    let mut registry = ShortcutRegistry::new();
    registry.register("FileCommand.NewFile", Chord::from_spec("Ctrl-Shift-N").unwrap(), false);
    let mut settings = Settings::empty();
    registry.save_to(&mut settings, Platform::MacOs).unwrap();
    let stored: BTreeMap<String, String> = settings.get("Shortcuts").unwrap();
    assert_eq!(stored["FileCommand.NewFile"], "\u{eae6}+Shift+N");
    let mut restored = ShortcutRegistry::new();
    restored.load_from(&settings, Platform::MacOs).unwrap();
    assert_eq!(restored, registry);
}

#[test]
fn load_overrides_defaults_and_ignores_empty_and_invalid_entries() {
    let settings = Settings::parse(
        br#"{"Shortcuts":{"EditCommand.Undo":"Ctrl+Alt+Z","FileCommand.NewFile":"","NodeCommand.CopyNode":"Hyper+C","NodeCommand.PasteNode":"Ctrl+Shift+V"}}"#,
    )
    .unwrap();
    let mut registry = ShortcutRegistry::with_defaults();
    let skipped = registry.load_from(&settings, Platform::Linux).unwrap();
    assert_eq!(skipped, vec!["NodeCommand.CopyNode"]);
    let undo = registry.binding_for("EditCommand.Undo").unwrap();
    assert_eq!(undo.chord.format(Platform::Linux), "Ctrl+Alt+Z");
    assert!(!undo.repeat, "overrides loaded from settings never repeat, as in D");
    assert_eq!(registry.binding_for("FileCommand.NewFile").unwrap().chord.format(Platform::Linux), "Ctrl+N");
    assert_eq!(registry.binding_for("NodeCommand.CopyNode").unwrap().chord.format(Platform::Linux), "Ctrl+C");
    assert_eq!(registry.binding_for("NodeCommand.PasteNode").unwrap().chord.format(Platform::Linux), "Ctrl+Shift+V");
    assert!(registry.command_for(&Chord::from_spec("Ctrl-Z").unwrap()).is_none());
}

#[test]
fn load_resolves_conflicts_against_existing_defaults() {
    let settings = Settings::parse(br#"{"Shortcuts":{"NodeCommand.AddNode":"Ctrl+Z"}}"#).unwrap();
    let mut registry = ShortcutRegistry::with_defaults();
    registry.load_from(&settings, Platform::Linux).unwrap();
    assert_eq!(registry.command_for(&Chord::from_spec("Ctrl-Z").unwrap()), Some("NodeCommand.AddNode"));
    assert!(registry.binding_for("EditCommand.Undo").is_none());
}

#[test]
fn load_without_shortcuts_key_changes_nothing_and_wrong_type_errors() {
    let mut registry = ShortcutRegistry::with_defaults();
    let before = registry.clone();
    assert!(registry.load_from(&Settings::parse(br#"{"a":1}"#).unwrap(), Platform::Linux).unwrap().is_empty());
    assert_eq!(registry, before);
    let bad = Settings::parse(br#"{"Shortcuts":["Ctrl+N"]}"#).unwrap();
    assert!(registry.load_from(&bad, Platform::Linux).unwrap_err().contains("Shortcuts"));
    let bad_value = Settings::parse(br#"{"Shortcuts":{"EditCommand.Undo":5}}"#).unwrap();
    assert!(registry.load_from(&bad_value, Platform::Linux).is_err());
    assert_eq!(registry, before);
}
