use soul2d_core::i18n::{
    current_locale_name, scan_locales, select_catalog, validate_culture_code, Catalog, LocaleEntry,
};
use std::path::{Path, PathBuf};

const HEADER: &str = "Content-Type: text/plain; charset=UTF-8\nLanguage: ja\nPlural-Forms: nplurals=1; plural=0;\n";
const FALLBACK_TL_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tl");

/// Builds a `.mo` like msgfmt does: header, originals table, translations table, NUL-terminated strings.
fn mo_bytes(pairs: &[(&str, &str)], big: bool) -> Vec<u8> {
    let word = |v: u32| if big { v.to_be_bytes() } else { v.to_le_bytes() };
    let n = pairs.len() as u32;
    let (orig_table, trans_table) = (28u32, 28 + 8 * n);
    let mut offset = 28 + 16 * n;
    let mut out = Vec::new();
    for w in [0x9504_12de_u32, 0, n, orig_table, trans_table, 0, 0] {
        out.extend(word(w));
    }
    let mut strings = Vec::new();
    for side in 0..2 {
        for (id, tr) in pairs {
            let s = if side == 0 { id } else { tr };
            out.extend(word(s.len() as u32));
            out.extend(word(offset));
            strings.extend(s.as_bytes());
            strings.push(0);
            offset += s.len() as u32 + 1;
        }
    }
    out.extend(strings);
    out
}

fn sample_pairs() -> Vec<(&'static str, &'static str)> {
    vec![
        ("", HEADER),
        ("File", "ファイル"),
        ("Open", "開く"),
        ("Untranslated", ""),
        ("LANG_NAME", "日本語"),
    ]
}

#[test]
fn mo_decodes_identically_in_both_endiannesses() {
    for big in [false, true] {
        let catalog = Catalog::from_mo(&mo_bytes(&sample_pairs(), big)).unwrap();
        assert_eq!(catalog.tr("File"), "ファイル", "big={big}");
        assert_eq!(catalog.tr("Open"), "開く");
        assert_eq!(catalog.tr("Untranslated"), "Untranslated");
        assert_eq!(catalog.tr("Not in catalogue"), "Not in catalogue");
        assert_eq!(catalog.tr(""), "");
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog.header_field("Language"), Some("ja"));
        assert_eq!(catalog.header_field("plural-forms"), Some("nplurals=1; plural=0;"));
        assert_eq!(catalog.language_name(), Some("日本語"));
    }
}

#[test]
fn mo_context_and_plural_keys_do_not_match_plain_msgids() {
    let pairs = [("Open", "開く"), ("menu\u{4}Open", "メニュー開く"), ("1 file\0%d files", "ファイル\0ファイルs")];
    let catalog = Catalog::from_mo(&mo_bytes(&pairs, false)).unwrap();
    assert_eq!(catalog.tr("Open"), "開く");
    assert_eq!(catalog.tr("1 file"), "1 file");
    assert_eq!(catalog.tr("menu\u{4}Open"), "メニュー開く");
}

#[test]
fn mo_rejects_malformed_input() {
    let good = mo_bytes(&sample_pairs(), false);
    assert!(Catalog::from_mo(&[]).unwrap_err().contains("truncated"));
    assert!(Catalog::from_mo(&good[..27]).unwrap_err().contains("truncated"));

    let mut bad_magic = good.clone();
    bad_magic[0] ^= 0xff;
    assert!(Catalog::from_mo(&bad_magic).unwrap_err().contains("magic"));

    let mut major = good.clone();
    major[6] = 1; // revision 0x0001_0000
    assert!(Catalog::from_mo(&major).unwrap_err().contains("revision"));

    let mut huge_count = good.clone();
    huge_count[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Catalog::from_mo(&huge_count).unwrap_err().contains("entries"));

    let mut table_oob = good.clone();
    table_oob[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Catalog::from_mo(&table_oob).unwrap_err().contains("table"));

    // First original: length u32::MAX at offset 28.
    let mut len_oob = good.clone();
    len_oob[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Catalog::from_mo(&len_oob).unwrap_err().contains("out of bounds"));

    let mut offset_oob = good.clone();
    offset_oob[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(Catalog::from_mo(&offset_oob).unwrap_err().contains("out of bounds"));

    let mut not_utf8 = mo_bytes(&[("A", "xx")], false);
    let last = not_utf8.len() - 2;
    not_utf8[last] = 0xff;
    assert!(Catalog::from_mo(&not_utf8).unwrap_err().contains("UTF-8"));

    let latin1 = mo_bytes(&[("", "Content-Type: text/plain; charset=ISO-8859-1\n")], false);
    assert!(Catalog::from_mo(&latin1).unwrap_err().contains("charset iso-8859-1"));
}

#[test]
fn every_truncation_of_a_mo_is_rejected_without_panicking() {
    for big in [false, true] {
        let good = mo_bytes(&sample_pairs(), big);
        for cut in 0..good.len() - 1 {
            assert!(Catalog::from_mo(&good[..cut]).is_err(), "prefix {cut} (big={big}) must fail");
        }
        assert!(Catalog::from_mo(&good[..good.len() - 1]).is_ok(), "only the final NUL is missing");
    }
}

#[test]
fn po_parses_multiline_strings_escapes_and_skips_untranslated() {
    let po = concat!(
        "# comment\n",
        "msgid \"\"\n",
        "msgstr \"\"\n",
        "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
        "\"Language: de\\n\"\n",
        "\n",
        "#: source/a.d:1\n",
        "#, c-format\n",
        "msgid \"Undo %s\"\n",
        "msgstr \"%s r\\u00fcckg\"\n",
    );
    // \u is not a gettext escape.
    assert!(Catalog::from_po(po).unwrap_err().contains("unknown escape"));

    let po = concat!(
        "msgid \"\"\n",
        "msgstr \"\"\n",
        "\"Content-Type: text/plain; charset=UTF-8\\n\"\n",
        "\"Language: de\\n\"\n",
        "\n",
        "#: source/a.d:1\n",
        "#, c-format\n",
        "msgid \"Undo %s\"\n",
        "msgstr \"%s zur\\\"ueck\"\n",
        "\n",
        "msgid \"\"\n",
        "\"Line one \"\n",
        "\"line two\\n\"\n",
        "msgstr \"\"\n",
        "\"Zeile eins \"\n",
        "\"Zeile\\tzwei\\n\"\n",
        "\n",
        "msgid \"Untranslated\"\n",
        "msgstr \"\"\n",
        "\n",
        "msgid \"Bytes\"\n",
        "msgstr \"\\303\\244\\x42\\\\\"\n",
    );
    let catalog = Catalog::from_po(po).unwrap();
    assert_eq!(catalog.header_field("Language"), Some("de"));
    assert_eq!(catalog.tr("Undo %s"), "%s zur\"ueck");
    assert_eq!(catalog.tr("Line one line two\n"), "Zeile eins Zeile\tzwei\n");
    assert_eq!(catalog.tr("Untranslated"), "Untranslated");
    assert_eq!(catalog.tr("Bytes"), "äB\\");
    assert_eq!(catalog.len(), 3);
}

#[test]
fn po_skips_fuzzy_and_obsolete_entries_but_keeps_context_and_plural_keys() {
    let po = concat!(
        "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n",
        "#, fuzzy\nmsgid \"Fuzzy\"\nmsgstr \"unsure\"\n\n",
        "#, fuzzy, c-format\n#~ msgid \"Gone\"\n#~ msgstr \"weg\"\n\n",
        "msgid \"After\"\nmsgstr \"danach\"\n\n",
        "msgctxt \"menu\"\nmsgid \"Open\"\nmsgstr \"Oeffnen\"\n\n",
        "msgid \"1 file\"\nmsgid_plural \"%d files\"\nmsgstr[0] \"eine Datei\"\nmsgstr[1] \"%d Dateien\"\n",
    );
    let catalog = Catalog::from_po(po).unwrap();
    assert_eq!(catalog.tr("Fuzzy"), "Fuzzy");
    assert_eq!(catalog.tr("Gone"), "Gone");
    assert_eq!(catalog.tr("After"), "danach");
    assert_eq!(catalog.tr("Open"), "Open");
    assert_eq!(catalog.tr("menu\u{4}Open"), "Oeffnen");
    assert_eq!(catalog.tr("1 file\0%d files"), "eine Datei\0%d Dateien");
    assert_eq!(catalog.len(), 3);
}

#[test]
fn po_rejects_malformed_input() {
    let cases: [(&str, &str); 8] = [
        ("msgid \"A\"\nmsgstr \"unterminated\n", "quoted string"),
        ("msgid \"A\"\nmsgstr \"bad\\q\"\n", "unknown escape"),
        ("msgid \"A\"\nmsgstr \"a\"b\"\n", "unescaped quote"),
        ("msgid \"A\"\nmsgstr \"a\\\"\n", "dangling backslash"),
        ("\"orphan\"\n", "without keyword"),
        ("msgstr \"x\"\n", "unexpected"),
        ("msgid \"A\"\n", "without msgstr"),
        ("msgid \"A\"\nmsgstr \"x\\x\"\n", "hex escape"),
    ];
    for (po, expect) in cases {
        let err = Catalog::from_po(po).unwrap_err();
        assert!(err.contains(expect), "{po:?}: {err}");
    }
    let latin1 = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=ISO-8859-1\\n\"\n";
    assert!(Catalog::from_po(latin1).unwrap_err().contains("charset"));
    let bad_index = "msgid \"a\"\nmsgid_plural \"b\"\nmsgstr[1] \"x\"\n";
    assert!(Catalog::from_po(bad_index).unwrap_err().contains("out of order"));
    let invalid_utf8 = "msgid \"A\"\nmsgstr \"\\377\"\n";
    assert!(Catalog::from_po(invalid_utf8).unwrap_err().contains("UTF-8"));
}

#[test]
fn culture_codes_follow_i18n_d() {
    for ok in ["C", "da", "da_DK", "de-DE", "de_DE@formal", "zh-CN", "ja"] {
        assert!(validate_culture_code(ok), "{ok}");
    }
    for bad in ["", "d", "daDK", "daaaaaDK", "da/DK", "zz_CH", "ch_ZZ", "dax", "da_", "日本"] {
        assert!(!validate_culture_code(bad), "{bad}");
    }
}

fn tl_dir() -> Option<PathBuf> {
    [Path::new(env!("CARGO_MANIFEST_DIR")).join("../tl"), PathBuf::from(FALLBACK_TL_DIR)]
        .into_iter()
        .find(|p| p.is_dir())
}

#[test]
fn real_ja_and_de_catalogues_load_directly_from_po() {
    let Some(tl) = tl_dir() else {
        eprintln!("skipped: no tl directory found next to the crate or at the fallback path");
        return;
    };
    let ja = Catalog::load(&tl.join("ja.po")).unwrap();
    let de = Catalog::load(&tl.join("de_DE.po")).unwrap();
    assert_eq!(ja.language_name(), Some("日本語"));
    assert_eq!(de.language_name(), Some("Deutsch"));
    assert_eq!(ja.header_field("Language"), Some("ja"));
    assert_eq!(ja.tr("File"), "ファイル");
    assert_eq!(de.tr("File"), "Datei");
    assert_eq!(ja.tr("Undo %s"), "\u{e894}%sを元に戻す");
    assert_eq!(de.tr("Open"), "Öffnen");
    assert_eq!(
        ja.tr("The layout of the mesh has changed, all deformations to this mesh will be deleted if you continue."),
        "メッシュのレイアウトが変更されたので、このメッシュに対するデフォーマの設定が全て解除されてしまいます。"
    );
    // msgstr "" in the catalogue: falls back to the msgid.
    let untranslated = "Oops! Your settings.json file is corrupted. Soul2D will load the default settings.\n";
    assert_eq!(ja.tr(untranslated), "設定ファイル(settings.json)が壊れているようです。デフォルトの設定を読み込んで利用します。\n");
    // Obsolete (#~) entries are not translations.
    assert_eq!(ja.tr("%s: vertex was removed."), "%s: vertex was removed.");
}

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soul2d-i18n-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn po_named(name: Option<&str>) -> String {
    let mut po = String::from("msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n");
    if let Some(name) = name {
        po.push_str(&format!("msgid \"LANG_NAME\"\nmsgstr \"{name}\"\n\n"));
    }
    po.push_str("msgid \"File\"\nmsgstr \"F\"\n");
    po
}

fn codes_and_names(entries: &[LocaleEntry]) -> Vec<(&str, &str)> {
    entries.iter().map(|e| (e.code.as_str(), e.name.as_str())).collect()
}

#[test]
fn scan_sorts_names_and_applies_fallbacks_and_disambiguation() {
    let first = unique_dir("scan-a");
    let second = unique_dir("scan-b");
    let write = |dir: &Path, name: &str, body: &[u8]| std::fs::write(dir.join(name), body).unwrap();
    write(&first, "ja.po", po_named(Some("日本語")).as_bytes());
    write(&first, "de_DE.po", po_named(Some("Deutsch")).as_bytes());
    write(&first, "ko.po", po_named(None).as_bytes()); // native-name estimate
    write(&first, "da.mo", &mo_bytes(&[("LANG_NAME", "Same")], true));
    write(&first, "it.po", po_named(Some("same")).as_bytes()); // same name, different case
    write(&first, "es.po", po_named(Some("Same")).as_bytes());
    write(&first, "zz.po", po_named(Some("Unknown code")).as_bytes());
    write(&first, "fr.mo", b"not a catalogue");
    write(&first, "notes.txt", b"ignored");
    // Same code in a later directory is ignored; a new code is added.
    write(&second, "ja.po", po_named(Some("Duplicate")).as_bytes());
    write(&second, "pt_BR.po", po_named(None).as_bytes());

    let entries = scan_locales(&[first.clone(), PathBuf::from("/nonexistent/soul2d-i18n"), second.clone()]);
    assert_eq!(
        codes_and_names(&entries),
        [
            ("de_DE", "Deutsch"),
            ("pt_BR", "Português"),
            ("da", "Same (da)"),
            ("es", "Same (es)"),
            ("it", "same"), // disambiguation compares names case-sensitively, like D
            ("ja", "日本語"),
            ("ko", "한국어"),
        ]
    );
    assert_eq!(entries[5].file, first.join("ja.po"));
    assert_eq!(entries[5].dir, first);

    // Selection by the settings `lang` value.
    assert_eq!(select_catalog(&entries, Some("ja")).tr("File"), "F");
    assert_eq!(select_catalog(&entries, Some("pt_BR")).tr("File"), "F");
    for english in [None, Some(""), Some("en"), Some("xx"), Some("fr")] {
        assert_eq!(select_catalog(&entries, english).tr("File"), "File", "{english:?}");
    }
    assert_eq!(current_locale_name(&entries, Some("ja")), "日本語");
    assert_eq!(current_locale_name(&entries, None), "English");
    assert_eq!(current_locale_name(&entries, Some("zh-TW")), "Chinese (Traditional)");
    assert_eq!(current_locale_name(&entries, Some("zh-CN")), "Chinese (Simplified)");
    assert_eq!(current_locale_name(&entries, Some("en_GB")), "English (United Kingdom)");

    let _ = std::fs::remove_dir_all(first);
    let _ = std::fs::remove_dir_all(second);
}

#[test]
fn scan_prefers_compiled_mo_over_po_with_the_same_code() {
    let dir = unique_dir("prefer-mo");
    std::fs::write(dir.join("ja.po"), po_named(Some("from po"))).unwrap();
    std::fs::write(dir.join("ja.mo"), mo_bytes(&[("LANG_NAME", "from mo")], false)).unwrap();
    let entries = scan_locales(&[dir.clone()]);
    assert_eq!(codes_and_names(&entries), [("ja", "from mo")]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn load_rejects_unknown_extension_and_missing_file() {
    assert!(Catalog::load(Path::new("/nonexistent/x.txt")).unwrap_err().contains("unsupported"));
    assert!(Catalog::load(Path::new("/nonexistent/x.mo")).is_err());
}

#[test]
fn the_c_culture_code_has_no_language_part_and_does_not_panic() {
    let dir = std::env::temp_dir().join(format!("soul2d-i18n-c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("C.po"), b"").unwrap();
    let entries = soul2d_core::i18n::scan_locales(&[dir.clone()]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(entries.len() <= 1);
    soul2d_core::i18n::current_locale_name(&[], Some("C"));
}
