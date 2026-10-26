//! Translation catalogues: GNU gettext `.mo` and `.po` readers, locale discovery and selection.
//!
//! Mirrors `source/soul2d/core/i18n.d` plus the i18n-d library it uses. D marks strings with
//! `_("...")` only (no context or plural forms), so lookup is by exact msgid with the msgid as fallback.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MO_MAGIC: u32 = 0x9504_12de;
const MO_MAGIC_SWAPPED: u32 = 0xde12_0495;
const MO_HEADER_LEN: usize = 28;
const MAX_ENTRIES: usize = 1 << 20;
const MAX_FILE_BYTES: u64 = 64 << 20;
const MAX_SCAN_FILES: usize = 4096;
/// msgid the D code translates to obtain a catalogue's own language name.
pub const LANG_NAME_MSGID: &str = "LANG_NAME";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    entries: HashMap<String, String>,
    header: String,
}

impl Catalog {
    /// English: every lookup falls back to the msgid.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Translation of `msgid`, or `msgid` itself when it is absent or untranslated.
    pub fn tr<'a>(&'a self, msgid: &'a str) -> &'a str {
        self.entries.get(msgid).map_or(msgid, String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Value of a header field such as `Language` or `Plural-Forms`.
    pub fn header_field(&self, name: &str) -> Option<&str> {
        header_field(&self.header, name)
    }

    /// The catalogue's own name for its language (translation of `LANG_NAME`), if it has one.
    pub fn language_name(&self) -> Option<&str> {
        self.entries.get(LANG_NAME_MSGID).map(String::as_str)
    }

    /// Loads `.mo` or `.po` by file extension.
    pub fn load(path: &Path) -> Result<Self, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext != "mo" && ext != "po" {
            return Err(format!("i18n: unsupported catalogue format {}", path.display()));
        }
        let len = std::fs::metadata(path).map_err(|e| format!("i18n: {}: {e}", path.display()))?.len();
        if len > MAX_FILE_BYTES {
            return Err(format!("i18n: {} is larger than {MAX_FILE_BYTES} bytes", path.display()));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("i18n: {}: {e}", path.display()))?;
        if ext == "mo" {
            Self::from_mo(&bytes)
        } else {
            let text = std::str::from_utf8(&bytes).map_err(|_| "i18n: .po is not valid UTF-8".to_string())?;
            Self::from_po(text)
        }
    }

    pub fn from_mo(data: &[u8]) -> Result<Self, String> {
        if data.len() < MO_HEADER_LEN {
            return Err("i18n: .mo header is truncated".into());
        }
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let big = match magic {
            MO_MAGIC => false,
            MO_MAGIC_SWAPPED => true,
            other => return Err(format!("i18n: bad .mo magic 0x{other:08x}")),
        };
        let word = |at: usize| -> u32 {
            let b = [data[at], data[at + 1], data[at + 2], data[at + 3]];
            if big {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            }
        };
        // The high half is the major revision; minor revisions only add optional fields.
        if word(4) >> 16 != 0 {
            return Err(format!("i18n: unsupported .mo revision 0x{:08x}", word(4)));
        }
        let count = word(8) as usize;
        if count > MAX_ENTRIES {
            return Err(format!("i18n: .mo has {count} entries, limit is {MAX_ENTRIES}"));
        }
        let (originals, translations) = (word(12) as usize, word(16) as usize);
        let string_at = |table: usize, index: usize| -> Result<&str, String> {
            let slot = table
                .checked_add(index * 8)
                .filter(|slot| slot.checked_add(8).is_some_and(|end| end <= data.len()))
                .ok_or("i18n: .mo string table is out of bounds")?;
            let (len, offset) = (word(slot) as usize, word(slot + 4) as usize);
            let end = offset.checked_add(len).filter(|end| *end <= data.len());
            let bytes = end.map(|end| &data[offset..end]).ok_or("i18n: .mo string is out of bounds")?;
            std::str::from_utf8(bytes).map_err(|_| "i18n: .mo string is not valid UTF-8".to_string())
        };
        let mut raw = Vec::with_capacity(count);
        for index in 0..count {
            raw.push((string_at(originals, index)?, string_at(translations, index)?));
        }
        Self::build(raw.into_iter().map(|(id, tr)| (id.to_string(), tr.to_string())))
    }

    pub fn from_po(text: &str) -> Result<Self, String> {
        Self::build(parse_po(text)?.into_iter())
    }

    // Keys follow the .mo convention: "ctxt\x04id" and "id\0plural", so plain lookups never match them.
    fn build(pairs: impl Iterator<Item = (String, String)>) -> Result<Self, String> {
        let mut catalog = Self::default();
        for (id, tr) in pairs {
            if id.is_empty() {
                catalog.header = tr;
            } else if !tr.is_empty() {
                catalog.entries.insert(id, tr);
            }
        }
        let charset = catalog
            .header_field("Content-Type")
            .and_then(|ct| ct.split(';').find_map(|p| p.trim().strip_prefix("charset=")))
            .map(|c| c.trim().to_ascii_lowercase());
        match charset.as_deref() {
            None | Some("utf-8" | "utf8" | "us-ascii" | "ascii" | "charset") => Ok(catalog),
            Some(other) => Err(format!("i18n: unsupported catalogue charset {other}")),
        }
    }
}

fn header_field<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

#[derive(Default)]
struct PoEntry {
    fuzzy: bool,
    context: Option<Vec<u8>>,
    id: Option<Vec<u8>>,
    plural: Option<Vec<u8>>,
    strs: Vec<Vec<u8>>,
}

enum PoField {
    Context,
    Id,
    Plural,
    Str(usize),
}

fn parse_po(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut entry = PoEntry::default();
    let mut field: Option<PoField> = None;
    for (n, raw) in text.lines().enumerate() {
        let at = n + 1;
        let line = raw.trim();
        let complete = !entry.strs.is_empty();
        if line.is_empty() || line.starts_with('#') {
            if complete {
                flush_po(&mut entry, &mut out)?;
                field = None;
            }
            if line.starts_with("#~") {
                entry = PoEntry::default(); // obsolete entry: drop its flags too
            } else if let Some(flags) = line.strip_prefix("#,") {
                entry.fuzzy |= flags.split(',').any(|f| f.trim() == "fuzzy");
            }
            continue;
        }
        if line.starts_with('"') {
            let bytes = unquote(line, at)?;
            let target = match field {
                Some(PoField::Context) => entry.context.as_mut(),
                Some(PoField::Id) => entry.id.as_mut(),
                Some(PoField::Plural) => entry.plural.as_mut(),
                Some(PoField::Str(i)) => entry.strs.get_mut(i),
                None => None,
            };
            target.ok_or_else(|| format!("i18n: .po line {at}: string without keyword"))?.extend(bytes);
            continue;
        }
        let (keyword, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim_start();
        if (keyword == "msgctxt" || keyword == "msgid") && complete {
            flush_po(&mut entry, &mut out)?;
        }
        let value = unquote(rest, at)?;
        match keyword {
            "msgctxt" if entry.context.is_none() && entry.id.is_none() => {
                entry.context = Some(value);
                field = Some(PoField::Context);
            }
            "msgid" if entry.id.is_none() => {
                entry.id = Some(value);
                field = Some(PoField::Id);
            }
            "msgid_plural" if entry.id.is_some() && entry.plural.is_none() && entry.strs.is_empty() => {
                entry.plural = Some(value);
                field = Some(PoField::Plural);
            }
            "msgstr" if entry.id.is_some() && entry.strs.is_empty() => {
                entry.strs.push(value);
                field = Some(PoField::Str(0));
            }
            k if k.starts_with("msgstr[") && k.ends_with(']') && entry.plural.is_some() => {
                let index: usize = k[7..k.len() - 1].parse().map_err(|_| format!("i18n: .po line {at}: bad index"))?;
                if index != entry.strs.len() || index >= 64 {
                    return Err(format!("i18n: .po line {at}: msgstr index out of order"));
                }
                entry.strs.push(value);
                field = Some(PoField::Str(index));
            }
            _ => return Err(format!("i18n: .po line {at}: unexpected `{keyword}`")),
        }
    }
    if !entry.strs.is_empty() {
        flush_po(&mut entry, &mut out)?;
    } else if entry.id.is_some() {
        return Err("i18n: .po ends inside an entry without msgstr".into());
    }
    Ok(out)
}

fn flush_po(entry: &mut PoEntry, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let e = std::mem::take(entry);
    let utf8 = |b: Vec<u8>| String::from_utf8(b).map_err(|_| "i18n: .po string is not valid UTF-8".to_string());
    let id = e.id.unwrap_or_default();
    if e.fuzzy && !id.is_empty() {
        return Ok(()); // fuzzy entries are not translations; a fuzzy header is still used
    }
    let mut key = Vec::new();
    if let Some(context) = e.context {
        key.extend(context);
        key.push(4);
    }
    key.extend(id);
    if let Some(plural) = e.plural {
        key.push(0);
        key.extend(plural);
    }
    let value = e.strs.join(&0u8);
    out.push((utf8(key)?, utf8(value)?));
    Ok(())
}

fn unquote(s: &str, at: usize) -> Result<Vec<u8>, String> {
    let bad = |what: &str| format!("i18n: .po line {at}: {what}");
    let body = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).ok_or_else(|| bad("expected quoted string"))?;
    let mut out = Vec::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            return Err(bad("unescaped quote"));
        }
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        let escaped = chars.next().ok_or_else(|| bad("dangling backslash"))?;
        out.push(match escaped {
            'n' => b'\n',
            't' => b'\t',
            'r' => b'\r',
            'a' => 7,
            'b' => 8,
            'f' => 12,
            'v' => 11,
            '"' => b'"',
            '\\' => b'\\',
            '\'' => b'\'',
            '0'..='7' => {
                let mut v = escaped.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                u8::try_from(v).map_err(|_| bad("octal escape out of range"))?
            }
            'x' => {
                let mut v = 0u32;
                let mut digits = 0;
                while let Some(d) = chars.peek().and_then(|d| d.to_digit(16)) {
                    v = (v * 16 + d) & 0xff;
                    digits += 1;
                    chars.next();
                }
                if digits == 0 {
                    return Err(bad("empty hex escape"));
                }
                v as u8
            }
            _ => return Err(bad("unknown escape")),
        });
    }
    Ok(out)
}

/// One selectable language: the catalogue file `file` found in `dir`, shown to users as `name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocaleEntry {
    pub code: String,
    pub name: String,
    pub file: PathBuf,
    pub dir: PathBuf,
}

/// Port of `i18nValidateCultureCode`: `xx`, or `xx_YY` / `xx-YY` (anything after the fifth char is ignored),
/// or the special code `C`.
pub fn validate_culture_code(code: &str) -> bool {
    if code == "C" {
        return true;
    }
    if !code.is_ascii() || code.len() < 2 || code.len() == 3 || code.len() == 4 {
        return false;
    }
    let language_ok = LANGUAGE_CODES.contains(&&code[..2]);
    if code.len() == 2 {
        return language_ok;
    }
    matches!(code.as_bytes()[2], b'-' | b'_') && language_ok && COUNTRY_CODES.contains(&&code[3..5])
}

fn lookup(table: &'static [(&str, &str)], key: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Name for a culture code with no catalogue of its own (`incGetCultureExpression` fallback).
fn culture_expression(code: &str) -> String {
    if !validate_culture_code(code) {
        return String::new();
    }
    let language = code.get(..2).and_then(|key| lookup(LANGUAGE_NAMES, key)).unwrap_or("");
    if code.len() < 5 {
        return language.to_string();
    }
    let region = match code {
        "zh-CN" => "Simplified",
        "zh-TW" => "Traditional",
        _ => lookup(COUNTRY_NAMES, &code[3..5]).unwrap_or(""),
    };
    format!("{language} ({region})")
}

/// Name shown for a catalogue: its `LANG_NAME` translation, else the native-name estimate for the code,
/// else the English culture expression.
fn language_name(catalog: &Catalog, code: &str) -> String {
    if let Some(name) = catalog.language_name() {
        return name.to_string();
    }
    if validate_culture_code(code) {
        if let Some(native) = code.get(..2).and_then(|key| lookup(LANGUAGE_NATIVE_NAMES, key)) {
            return native.to_string();
        }
    }
    culture_expression(code)
}

/// Discovers `*.mo` and `*.po` catalogues in `dirs` (earlier directories win a duplicate code), then sorts
/// them case-insensitively by name like `incLocaleInit`. Missing directories and unreadable or malformed
/// files are skipped.
pub fn scan_locales(dirs: &[PathBuf]) -> Vec<LocaleEntry> {
    let mut found: Vec<LocaleEntry> = Vec::new();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else { continue };
        let mut files: Vec<PathBuf> = read
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("mo" | "po")) && p.is_file())
            .take(MAX_SCAN_FILES)
            .collect();
        // Compiled .mo wins over a .po with the same code, then name order for determinism.
        files.sort_by_key(|p| (p.file_stem().map(|s| s.to_owned()), p.extension().map(|s| s.to_owned())));
        for file in files {
            let Some(code) = file.file_stem().and_then(|s| s.to_str()) else { continue };
            if !validate_culture_code(code) || found.iter().any(|e| e.code == code) {
                continue;
            }
            let Ok(catalog) = Catalog::load(&file) else { continue };
            found.push(LocaleEntry {
                code: code.to_string(),
                name: language_name(&catalog, code),
                file: file.clone(),
                dir: dir.clone(),
            });
        }
    }
    found.sort_by(compare_entries);
    mark_dups(&mut found);
    found
}

fn compare_entries(a: &LocaleEntry, b: &LocaleEntry) -> Ordering {
    let fold = |s: &str| s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect::<Vec<_>>();
    fold(&a.name)
        .cmp(&fold(&b.name))
        .then_with(|| a.dir.cmp(&b.dir))
        .then_with(|| a.code.cmp(&b.code))
}

/// Appends ` (code)` to every name shared by several entries. Expects `entries` sorted by name.
fn mark_dups(entries: &mut [LocaleEntry]) {
    let mut dup = vec![false; entries.len()];
    for i in 1..entries.len() {
        if entries[i].name == entries[i - 1].name {
            dup[i] = true;
            dup[i - 1] = true;
        }
    }
    for (entry, dup) in entries.iter_mut().zip(dup) {
        if dup {
            entry.name = format!("{} ({})", entry.name, entry.code);
        }
    }
}

/// Catalogue for the settings `lang` value (`incLocaleInitFromSettings`). A missing key, `""`, `"en"`,
/// an undiscovered code, or an unreadable file all give the English (empty) catalogue.
pub fn select_catalog(entries: &[LocaleEntry], lang: Option<&str>) -> Catalog {
    let Some(lang) = lang.filter(|l| !l.is_empty() && *l != "en") else { return Catalog::empty() };
    entries
        .iter()
        .find(|e| e.code == lang)
        .and_then(|e| Catalog::load(&e.file).ok())
        .unwrap_or_default()
}

/// Human-readable name of the current setting (`incLocaleCurrentName`); `lang` defaults to `"en"`.
pub fn current_locale_name(entries: &[LocaleEntry], lang: Option<&str>) -> String {
    let code = lang.filter(|l| !l.is_empty()).unwrap_or("en");
    match entries.iter().find(|e| e.code == code) {
        Some(entry) => entry.name.clone(),
        None => culture_expression(code),
    }
}

// Generated from i18n-d culture.d (codes, English names, native-name estimates).
const COUNTRY_CODES: &[&str] = &[
    "AD",
    "AE",
    "AF",
    "AG",
    "AI",
    "AL",
    "AM",
    "AO",
    "AQ",
    "AR",
    "AS",
    "AT",
    "AU",
    "AW",
    "AX",
    "AZ",
    "BA",
    "BB",
    "BD",
    "BE",
    "BF",
    "BG",
    "BH",
    "BI",
    "BJ",
    "BL",
    "BM",
    "BN",
    "BO",
    "BQ",
    "BR",
    "BS",
    "BT",
    "BV",
    "BW",
    "BY",
    "BZ",
    "CA",
    "CC",
    "CD",
    "CF",
    "CG",
    "CH",
    "CI",
    "CK",
    "CL",
    "CM",
    "CN",
    "CO",
    "CR",
    "CU",
    "CV",
    "CW",
    "CX",
    "CY",
    "CZ",
    "DE",
    "DJ",
    "DK",
    "DO",
    "DZ",
    "EC",
    "EE",
    "EG",
    "EH",
    "ER",
    "ES",
    "ET",
    "FI",
    "FJ",
    "FK",
    "FM",
    "FO",
    "FR",
    "GA",
    "GB",
    "GD",
    "GE",
    "GF",
    "GG",
    "GH",
    "GI",
    "GL",
    "GM",
    "GN",
    "GP",
    "GQ",
    "GR",
    "GS",
    "GT",
    "GU",
    "GW",
    "GY",
    "HK",
    "HM",
    "HN",
    "HR",
    "HT",
    "HU",
    "ID",
    "IE",
    "IL",
    "IM",
    "IN",
    "IO",
    "IQ",
    "IR",
    "IS",
    "IT",
    "JE",
    "JM",
    "JO",
    "JP",
    "KE",
    "KG",
    "KH",
    "KI",
    "KM",
    "KN",
    "KP",
    "KR",
    "KW",
    "KY",
    "KZ",
    "LA",
    "LB",
    "LC",
    "LI",
    "LK",
    "LR",
    "LS",
    "LT",
    "LU",
    "LV",
    "LY",
    "MA",
    "MC",
    "MD",
    "ME",
    "MF",
    "MG",
    "MH",
    "MK",
    "ML",
    "MM",
    "MN",
    "MO",
    "MP",
    "MQ",
    "MR",
    "MS",
    "MT",
    "MU",
    "MV",
    "MW",
    "MX",
    "MY",
    "MZ",
    "NA",
    "NC",
    "NE",
    "NF",
    "NG",
    "NI",
    "NL",
    "NO",
    "NP",
    "NR",
    "NU",
    "NZ",
    "OM",
    "PA",
    "PE",
    "PF",
    "PG",
    "PH",
    "PK",
    "PL",
    "PM",
    "PN",
    "PR",
    "PS",
    "PT",
    "PW",
    "PY",
    "QA",
    "RE",
    "RO",
    "RS",
    "RU",
    "RW",
    "SA",
    "SB",
    "SC",
    "SD",
    "SE",
    "SG",
    "SH",
    "SI",
    "SJ",
    "SK",
    "SL",
    "SM",
    "SN",
    "SO",
    "SR",
    "SS",
    "ST",
    "SV",
    "SX",
    "SY",
    "SZ",
    "TC",
    "TD",
    "TF",
    "TG",
    "TH",
    "TJ",
    "TK",
    "TL",
    "TM",
    "TN",
    "TO",
    "TR",
    "TT",
    "TV",
    "TW",
    "TZ",
    "UA",
    "UG",
    "UM",
    "US",
    "UY",
    "UZ",
    "VA",
    "VC",
    "VE",
    "VG",
    "VI",
    "VN",
    "VU",
    "WF",
    "WS",
    "YE",
    "YT",
    "ZA",
    "ZM",
    "ZW",
];
const LANGUAGE_CODES: &[&str] = &[
    "aa",
    "ab",
    "ae",
    "af",
    "ak",
    "am",
    "an",
    "ar",
    "as",
    "av",
    "ay",
    "az",
    "ba",
    "be",
    "bg",
    "bh",
    "bi",
    "bm",
    "bn",
    "bo",
    "br",
    "bs",
    "ca",
    "ce",
    "ch",
    "co",
    "cr",
    "cs",
    "cu",
    "cv",
    "cy",
    "da",
    "de",
    "dv",
    "dz",
    "ee",
    "el",
    "en",
    "eo",
    "es",
    "et",
    "eu",
    "fa",
    "ff",
    "fi",
    "fj",
    "fo",
    "fr",
    "fy",
    "ga",
    "gd",
    "gl",
    "gn",
    "gu",
    "gv",
    "ha",
    "he",
    "hi",
    "ho",
    "hr",
    "ht",
    "hu",
    "hy",
    "hz",
    "ia",
    "id",
    "ie",
    "ig",
    "ii",
    "ik",
    "io",
    "is",
    "it",
    "iu",
    "ja",
    "jv",
    "ka",
    "kg",
    "ki",
    "kj",
    "kk",
    "kl",
    "km",
    "kn",
    "ko",
    "kr",
    "ks",
    "ku",
    "kv",
    "kw",
    "ky",
    "la",
    "lb",
    "lg",
    "li",
    "ln",
    "lo",
    "lt",
    "lu",
    "lv",
    "mg",
    "mh",
    "mi",
    "mk",
    "ml",
    "mn",
    "mr",
    "ms",
    "mt",
    "my",
    "na",
    "nb",
    "nd",
    "ne",
    "ng",
    "nl",
    "nn",
    "no",
    "nr",
    "nv",
    "ny",
    "oc",
    "oj",
    "om",
    "or",
    "os",
    "pa",
    "pi",
    "pl",
    "ps",
    "pt",
    "qu",
    "rm",
    "rn",
    "ro",
    "ru",
    "rw",
    "sa",
    "sc",
    "sd",
    "se",
    "sg",
    "si",
    "sk",
    "sl",
    "sm",
    "sn",
    "so",
    "sq",
    "sr",
    "ss",
    "st",
    "su",
    "sv",
    "sw",
    "ta",
    "te",
    "tg",
    "th",
    "ti",
    "tk",
    "tl",
    "tn",
    "to",
    "tr",
    "ts",
    "tt",
    "tw",
    "ty",
    "ug",
    "uk",
    "ur",
    "uz",
    "ve",
    "vi",
    "vo",
    "wa",
    "wo",
    "xh",
    "yi",
    "yo",
    "za",
    "zh",
    "zu",
];
const COUNTRY_NAMES: &[(&str, &str)] = &[
    ("AD", "Andorra"),
    ("AE", "United Arab Emirates"),
    ("AF", "Afghanistan"),
    ("AG", "Antigua and Barbuda"),
    ("AI", "Anguilla"),
    ("AL", "Albania"),
    ("AM", "Armenia"),
    ("AO", "Angola"),
    ("AQ", "Antarctica"),
    ("AR", "Argentina"),
    ("AS", "American Samoa"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("AW", "Aruba"),
    ("AX", "Aaland Islands"),
    ("AZ", "Azerbaijan"),
    ("BA", "Bosnia & Herzegovina"),
    ("BB", "Barbados"),
    ("BD", "Bangladesh"),
    ("BE", "Belgium"),
    ("BF", "Burkina Faso"),
    ("BG", "Bulgaria"),
    ("BH", "Bahrain"),
    ("BI", "Burundi"),
    ("BJ", "Benin"),
    ("BL", "Saint Barthelemy"),
    ("BM", "Bermuda"),
    ("BN", "Brunei Darussalam"),
    ("BO", "Bolivia"),
    ("BQ", "Bonaire, Sint Eustatius & Saba"),
    ("BR", "Brazil"),
    ("BS", "Bahamas"),
    ("BT", "Bhutan"),
    ("BV", "Bouvet Island"),
    ("BW", "Botswana"),
    ("BY", "Belarus"),
    ("BZ", "Belize"),
    ("CA", "Canada"),
    ("CC", "Cocos Islands"),
    ("CD", "Democratic Republic of Congo"),
    ("CF", "Central African Republic"),
    ("CG", "Congo"),
    ("CH", "Switzerland"),
    ("CI", "Côte d'Ivoire"),
    ("CK", "Cook Islands"),
    ("CL", "Chile"),
    ("CM", "Cameroon"),
    ("CN", "China"),
    ("CO", "Colombia"),
    ("CR", "Costa Rica"),
    ("CU", "Cuba"),
    ("CV", "Cape Verde"),
    ("CW", "Curaçao"),
    ("CX", "Christmas Island"),
    ("CY", "Cyprus"),
    ("CZ", "Czech Republic"),
    ("DE", "Germany"),
    ("DJ", "Djibouti"),
    ("DK", "Denmark"),
    ("DM", "Dominica"),
    ("DO", "Dominican Republic"),
    ("DZ", "Algeria"),
    ("EC", "Ecuador"),
    ("EE", "Estonia"),
    ("EG", "Egypt"),
    ("EH", "Sahrawi Arab Democratic Republic"),
    ("ER", "Eritrea"),
    ("ES", "Spain"),
    ("ET", "Ethiopia"),
    ("FI", "Finland"),
    ("FJ", "Fiji"),
    ("FK", "Falkland Islands (Malvinas)"),
    ("FM", "Federated States of Micronesia"),
    ("FO", "Faroe Islands"),
    ("FR", "France"),
    ("GA", "Gabon"),
    ("GB", "United Kingdom"),
    ("GD", "Grenada"),
    ("GE", "Georgia"),
    ("GF", "French Guiana"),
    ("GG", "Guernsey"),
    ("GH", "Ghana"),
    ("GI", "Gibraltar"),
    ("GL", "Greenland"),
    ("GM", "Gambia"),
    ("GN", "Guinea"),
    ("GP", "Guadaloupe"),
    ("GQ", "Equatorial Guinea"),
    ("GR", "Greece"),
    ("GS", "South Georgia & South Sandwich Islands"),
    ("GT", "Guatemala"),
    ("GU", "Guam"),
    ("GW", "Guinea-Bissau"),
    ("GY", "Guyana"),
    ("HK", "Hong Kong"),
    ("HM", "Heard Island & McDonald Islands"),
    ("HN", "Honduras"),
    ("HR", "Croatia"),
    ("HT", "Haiti"),
    ("HU", "Hungary"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IM", "Isle of Man"),
    ("IN", "India"),
    ("IO", "British Indian Ocean Territory"),
    ("IQ", "Iraq"),
    ("IR", "Islamic Republic of Iran"),
    ("IS", "Iceland"),
    ("IT", "Italy"),
    ("JE", "Jersey"),
    ("JM", "Jamaica"),
    ("JO", "Jordan"),
    ("JP", "Japan"),
    ("KE", "Kenya"),
    ("KG", "Kyrgyzstan"),
    ("KH", "Cambodia"),
    ("KI", "Kiribati"),
    ("KM", "Comoros"),
    ("KN", "Saint Kitts & Nevis"),
    ("KP", "Democratic People's Republic of Korea"),
    ("KR", "Republic of Korea"),
    ("KW", "Kuwait"),
    ("KY", "Cayman Islands"),
    ("KZ", "Kazakhstan"),
    ("LA", "Lao People's Democratic Republic"),
    ("LB", "Lebalon"),
    ("LC", "Saint Lucia"),
    ("LI", "Lichtenstein"),
    ("LK", "Sri Lanka"),
    ("LR", "Liberia"),
    ("LS", "Lesotho"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("LV", "Latvia"),
    ("LY", "Libya"),
    ("MA", "Morocco"),
    ("MC", "Monaco"),
    ("MD", "Republic of Moldova"),
    ("ME", "Montenegro"),
    ("MF", "Saint Martin (French part)"),
    ("MG", "Madagascar"),
    ("MH", "Marshall Islands"),
    ("MK", "North Macedonia"),
    ("ML", "Mali"),
    ("MM", "Myanmar"),
    ("MN", "Mongolia"),
    ("MO", "Macao"),
    ("MP", "Northen Marina Islands"),
    ("MQ", "Martinique"),
    ("MR", "Mauritania"),
    ("MS", "Montserrat"),
    ("MT", "Malta"),
    ("MU", "Mauritius"),
    ("MV", "Maldives"),
    ("MW", "Malawi"),
    ("MX", "Mexico"),
    ("MY", "Malaysia"),
    ("MZ", "Mozambique"),
    ("NA", "Namibia"),
    ("NC", "New Caledonia"),
    ("NE", "Niger"),
    ("NF", "Norfolk Island"),
    ("NG", "Nigeria"),
    ("NI", "Nicaragua"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NP", "Nepal"),
    ("NR", "Nauru"),
    ("NU", "Niue"),
    ("NZ", "New Zealand"),
    ("OM", "Oman"),
    ("PA", "Panama"),
    ("PE", "Peru"),
    ("PF", "French Polynesia"),
    ("PG", "Papua New Guinea"),
    ("PH", "Phillipines"),
    ("PK", "Pakistan"),
    ("PL", "Poland"),
    ("PM", "Saint Pierre & Miquelon"),
    ("PN", "Pitcairn"),
    ("PR", "Puerto Rico"),
    ("PS", "Palestine"),
    ("PT", "Portugal"),
    ("PW", "Palau"),
    ("PY", "Paraguay"),
    ("QA", "Qatar"),
    ("RE", "Reunion"),
    ("RO", "Romania"),
    ("RS", "Serbia"),
    ("RU", "Russian Federation"),
    ("RW", "Rwanda"),
    ("SA", "Saudi Arabia"),
    ("SB", "Solomon Islands"),
    ("SC", "Seychelles"),
    ("SD", "Sudan"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("SH", "Saint Helena, Ascension & Tristan da Cunha"),
    ("SI", "Slovenia"),
    ("SJ", "Svalbard & Jan Mayen"),
    ("SK", "Slovakia"),
    ("SL", "Sierra Leone"),
    ("SM", "San Marino"),
    ("SN", "Senegal"),
    ("SO", "Somalia"),
    ("SR", "Suriname"),
    ("SS", "South Sudan"),
    ("ST", "Sao Tome & Principe"),
    ("SV", "El Salvador"),
    ("SX", "Sint Maarten (Dutch part)"),
    ("SY", "Syrian Arab Republic"),
    ("SZ", "Swaziland"),
    ("TC", "Turks & Caicos Islands"),
    ("TD", "Chad"),
    ("TF", "French Southern Territories"),
    ("TG", "Togo"),
    ("TH", "Thailand"),
    ("TJ", "Tajikistan"),
    ("TK", "Tokelau"),
    ("TL", "Timor-Leste"),
    ("TM", "Turkmenistan"),
    ("TN", "Tunisia"),
    ("TO", "Tonga"),
    ("TR", "Turkey"),
    ("TT", "Trinidad & Tobago"),
    ("TV", "Tuvalu"),
    ("TW", "Taiwan"),
    ("TZ", "United Republic of Tanzania"),
    ("UA", "Ukraine"),
    ("UG", "Uganda"),
    ("UM", "United States Minor Outlying Islands"),
    ("US", "United States"),
    ("UY", "Uruguay"),
    ("UZ", "Uzbekistan"),
    ("VA", "Holy See (Vatican City State)"),
    ("VC", "Saint Vincent & Grenadines"),
    ("VE", "Bolivarian Republic of Venezuela"),
    ("VG", "British Virgin Islands"),
    ("VI", "US Virgin Islands"),
    ("VN", "Vietnam"),
    ("VU", "Vanuatu"),
    ("WF", "Wallis & Futuna"),
    ("WS", "Samoa"),
    ("YE", "Yemen"),
    ("YT", "Mayotte"),
    ("ZA", "South Africa"),
    ("ZM", "Zambia"),
    ("ZW", "Zimbabwe"),
];
const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("aa", "Afar"),
    ("ab", "Abkahzian"),
    ("ae", "Avestan"),
    ("af", "Afrikaans"),
    ("ak", "Akan"),
    ("am", "Amharic"),
    ("an", "Aragonese"),
    ("ar", "Arabic"),
    ("as", "Assamese"),
    ("av", "Avaric"),
    ("ay", "Aymara"),
    ("az", "Azerbaijani"),
    ("ba", "Bashkir"),
    ("be", "Belarusian"),
    ("bg", "Bulgarian"),
    ("bh", "Bihari"),
    ("bi", "Bislama"),
    ("bm", "Bambara"),
    ("bn", "Bengali"),
    ("bo", "Tibetan"),
    ("br", "Breton"),
    ("bs", "Bosnian"),
    ("ca", "Catlan"),
    ("ce", "Chechen"),
    ("ch", "Chamorro"),
    ("co", "Corsican"),
    ("cr", "Cree"),
    ("cs", "Czech"),
    ("cu", "Church Slavic"),
    ("cv", "Chuvash"),
    ("cy", "Welsh"),
    ("da", "Danish"),
    ("de", "German"),
    ("dv", "Divehi"),
    ("dz", "Dzongkha"),
    ("ee", "Ewe"),
    ("el", "Greek"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Spanish"),
    ("et", "Estonian"),
    ("eu", "Basque"),
    ("fa", "Persian"),
    ("ff", "Fulah"),
    ("fi", "Finnish"),
    ("fj", "Fijian"),
    ("fo", "Faroese"),
    ("fr", "French"),
    ("fy", "Western Frisian"),
    ("ga", "Irish"),
    ("gd", "Gaelic"),
    ("gl", "Galacian"),
    ("gn", "Guarani"),
    ("gu", "Gujarati"),
    ("gv", "Manx"),
    ("ha", "Hausa"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("ho", "Hiri Motu"),
    ("hr", "Croatian"),
    ("ht", "Haitian"),
    ("hu", "Hungarian"),
    ("hy", "Armenian"),
    ("hz", "Herero"),
    ("ia", "Interlingua"),
    ("id", "Indonesian"),
    ("ie", "Interlingue"),
    ("ig", "Igbo"),
    ("ii", "Sichuan Yi"),
    ("ik", "Inupiak"),
    ("io", "Ido"),
    ("is", "Icelandic"),
    ("it", "Italian"),
    ("iu", "Inuktitut"),
    ("ja", "Japanese"),
    ("jv", "Javanese"),
    ("ka", "Georgian"),
    ("kg", "Kongo"),
    ("ki", "Kikuyu"),
    ("kj", "Kuanyama"),
    ("kk", "Kazakh"),
    ("kl", "Greenlandic"),
    ("km", "Central Khmer"),
    ("kn", "Kannada"),
    ("ko", "Korean"),
    ("kr", "Kanuri"),
    ("ks", "Kashmiri"),
    ("ku", "Kurdish"),
    ("kv", "Komi"),
    ("kw", "Cornish"),
    ("ky", "Kirghiz"),
    ("la", "Latin"),
    ("lb", "Luxembourgish"),
    ("lg", "Ganda"),
    ("li", "Limburgish"),
    ("ln", "Lingala"),
    ("lo", "Lao"),
    ("lt", "Lithuanian"),
    ("lu", "Luba-Katanga"),
    ("lv", "Latvian"),
    ("mg", "Malagasy"),
    ("mh", "Marshallese"),
    ("mi", "Maori"),
    ("mk", "Macedonian"),
    ("ml", "Malayalam"),
    ("mn", "Mongolian"),
    ("mr", "Marathi"),
    ("ms", "Malay"),
    ("mt", "Maltese"),
    ("my", "Burmese"),
    ("na", "Nauru"),
    ("nb", "Norwegian Bokmål"),
    ("nd", "North Ndebele"),
    ("ne", "Nepali"),
    ("ng", "Ndonga"),
    ("nl", "Dutch"),
    ("nn", "Norwegian Nynorsk"),
    ("no", "Norwegian"),
    ("nr", "South Ndebele"),
    ("nv", "Navajo"),
    ("ny", "Chichewa"),
    ("oc", "Occitan"),
    ("oj", "Ojibwa"),
    ("om", "Omoro"),
    ("or", "Oriya"),
    ("os", "Ossetian"),
    ("pa", "Panjabi"),
    ("pi", "Pali"),
    ("pl", "Polish"),
    ("ps", "Pushto"),
    ("pt", "Portugese"),
    ("qu", "Quechua"),
    ("rm", "Romansh"),
    ("rn", "Rundi"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("rw", "Kinyarwanda"),
    ("sa", "Sanskrit"),
    ("sc", "Sardinian"),
    ("sd", "Sindhi"),
    ("se", "Northen Sami"),
    ("sg", "Sango"),
    ("si", "Sinhala"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("sm", "Samoan"),
    ("sn", "Shona"),
    ("so", "Somali"),
    ("sq", "Albanian"),
    ("sr", "Serbian"),
    ("ss", "Swati"),
    ("st", "Southern Sotho"),
    ("su", "Sundanese"),
    ("sv", "Swedish"),
    ("sw", "Swahili"),
    ("ta", "Tamil"),
    ("te", "Tegulu"),
    ("tg", "Tajik"),
    ("th", "Thai"),
    ("ti", "Tigrinya"),
    ("tk", "Turkmen"),
    ("tl", "Tagalog"),
    ("tn", "Tswana"),
    ("to", "Tonga"),
    ("tr", "Turkish"),
    ("ts", "Tsonga"),
    ("tt", "Tatar"),
    ("tw", "Twi"),
    ("ty", "Tahitian"),
    ("ug", "Ughur"),
    ("uk", "Ukranian"),
    ("ur", "Urdu"),
    ("uz", "Uzbek"),
    ("ve", "Venda"),
    ("vi", "Vietnamese"),
    ("vo", "Volapük"),
    ("wa", "Walloon"),
    ("wo", "Wolof"),
    ("xh", "Xhosa"),
    ("yi", "Yiddish"),
    ("yo", "Yoruba"),
    ("za", "Zhuang"),
    ("zh", "Chinese"),
    ("zu", "Zulu"),
];
const LANGUAGE_NATIVE_NAMES: &[(&str, &str)] = &[
    ("aa", "Qafaraf"),
    ("ab", "Аҧсуа бызшәа"),
    ("ae", "𐬛𐬍𐬥 𐬛𐬀𐬠𐬌𐬫𐬭𐬵"),
    ("af", "Afrikaans"),
    ("ak", "Akan"),
    ("am", "ኣማርኛ"),
    ("an", "Aragonés"),
    ("ar", "عربية يهودية"),
    ("as", "অসমীয়া"),
    ("av", "Avaric"),
    ("ay", "Aymar aru"),
    ("az", "Azərbaycan dili"),
    ("ba", "Башҡортса"),
    ("be", "Беларуская мова"),
    ("bg", "Български"),
    ("bh", "Bihari"),
    ("bi", "Bislama"),
    ("bm", "Bamanankan"),
    ("bn", "বাংলা"),
    ("bo", "ཁམས་སྐད"),
    ("br", "Brezhoneg"),
    ("bs", "Bosanski"),
    ("ca", "Catlan"),
    ("ce", "Нохчийн мотт"),
    ("ch", "Finuʼ Chamoru"),
    ("co", "Corsu"),
    ("cr", "Cree"),
    ("cs", "Čeština"),
    ("cu", "Church Slavic"),
    ("cv", "Чӑвашла"),
    ("cy", "Cymraeg"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("dv", "Divehi"),
    ("dz", "རྫོང་ཁ་"),
    ("ee", "Eʋegbe"),
    ("el", "ελληνικά"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Español"),
    ("et", "Eesti keel"),
    ("eu", "Euskara"),
    ("fa", "فارسی"),
    ("ff", "Fulah"),
    ("fi", "Suomi"),
    ("fj", "Na vosa vaka-Viti"),
    ("fo", "Føroyskt mál"),
    ("fr", "Français"),
    ("fy", "Frysk"),
    ("ga", "Gaeilge"),
    ("gd", "Gàidhlig"),
    ("gl", "Galacian"),
    ("gn", "Avañeʼẽ"),
    ("gu", "ગુજરાતી"),
    ("gv", "Gaelg"),
    ("ha", "هَرْشَن هَوْسَ"),
    ("he", "עברית"),
    ("hi", "हिन्दी "),
    ("ho", "Hiri Motu"),
    ("hr", "Hrvatski"),
    ("ht", "Kreyòl ayisyen"),
    ("hu", "Magyar"),
    ("hy", "Հայերէն"),
    ("hz", "Otjiherero"),
    ("ia", "Interlingua"),
    ("id", "Bahasa Indonesia"),
    ("ie", "Interlingue"),
    ("ig", "Ásụ̀sụ̀ Ìgbò"),
    ("ii", "ꆈꌠ꒿"),
    ("ik", "Inupiak"),
    ("io", "Ido"),
    ("is", "Íslenska"),
    ("it", "Italiano"),
    ("iu", "ᐃᓄᒃᑎᑐᑦ"),
    ("ja", "日本語"),
    ("jv", "Basa Jawa"),
    ("ka", "ქართული ენა"),
    ("kg", "Kikongo"),
    ("ki", "Gĩkũyũ"),
    ("kj", "Kuanyama"),
    ("kk", "Қазақ тілі"),
    ("kl", "Kalaallisut"),
    ("km", "ភាសាខ្មែរ "),
    ("kn", "ಕನ್ನಡ"),
    ("ko", "한국어"),
    ("kr", "Kànùrí"),
    ("ks", "कॉशुर"),
    ("ku", "کوردی"),
    ("kv", "Коми кыв"),
    ("kw", "Kernewek"),
    ("ky", "Kirghiz"),
    ("la", "Lingua Latina"),
    ("lb", "Lëtzebuergesch"),
    ("lg", "Oluganda"),
    ("li", "Limburgs"),
    ("ln", "Lingala"),
    ("lo", "ພາສາລາວ"),
    ("lt", "Lietuvių kalba"),
    ("lu", "Luba-Katanga"),
    ("lv", "Latviešu valoda"),
    ("mg", "Malagasy"),
    ("mh", "Kajin M̧ajeļ"),
    ("mi", "Māori"),
    ("mk", "македонски"),
    ("ml", "മലയാളം"),
    ("mn", "монгол"),
    ("mr", "मराठी"),
    ("ms", "Bahasa Melayu"),
    ("mt", "Malti"),
    ("my", "မြန်မာဘာသာ"),
    ("na", "Nauru"),
    ("nb", "Bokmål"),
    ("nd", "isiNdebele"),
    ("ne", "नेपाली"),
    ("ng", "Ndonga"),
    ("nl", "Nederlands"),
    ("nn", "Nynorsk"),
    ("no", "Norsk"),
    ("nr", "isiNdebele"),
    ("nv", "Diné Bizaad"),
    ("ny", "Chicheŵa"),
    ("oc", "Occitan"),
    ("oj", "Ojibwa"),
    ("om", "Omoro"),
    ("or", "Oriya"),
    ("os", "ирон ӕвзаг"),
    ("pa", "ਪੰਜਾਬੀ"),
    ("pi", "पालि"),
    ("pl", "Polski"),
    ("ps", "Pushto"),
    ("pt", "Português"),
    ("qu", "Kechua"),
    ("rm", "Rumantsch"),
    ("rn", "íkiRǔndi"),
    ("ro", "Vlășéște "),
    ("ru", "Русский язык"),
    ("rw", "Ikinyarwanda"),
    ("sa", "संस्कृत"),
    ("sc", "Sardu"),
    ("sd", "سنڌي"),
    ("se", "Davvisámegiella"),
    ("sg", "Yângâ tî sängö"),
    ("si", "සිංහල"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("sm", "Gagana fa‘a Sāmoa"),
    ("sn", "chiShona"),
    ("so", "af Soomaali"),
    ("sq", "Shqip"),
    ("sr", "српски"),
    ("ss", "siSwati"),
    ("st", "seSotho"),
    ("su", "Basa Sunda"),
    ("sv", "Svenska"),
    ("sw", "Kiswahili "),
    ("ta", "தமிழ்"),
    ("te", "Tegulu"),
    ("tg", "тоҷикӣ"),
    ("th", "ภาษาไทย"),
    ("ti", "ትግርኛ"),
    ("tk", "Türkmen dili"),
    ("tl", "Tagalog"),
    ("tn", "Setswana"),
    ("to", "Chitonga"),
    ("tr", "Türkçe"),
    ("ts", "Xitsonga"),
    ("tt", "Tatarça"),
    ("tw", "Twi"),
    ("ty", "Reo Tahiti"),
    ("ug", "ئۇيغۇر"),
    ("uk", "украї́нська мо́ва"),
    ("ur", "اُردُو"),
    ("uz", "Ўзбек тили"),
    ("ve", "Tshivenḓa"),
    ("vi", "Tiếng Việt"),
    ("vo", "Volapük"),
    ("wa", "Walon"),
    ("wo", "Wolof"),
    ("xh", "isiXhosa"),
    ("yi", "ייִדיש"),
    ("yo", "Èdè Yorùbá"),
    ("za", "Vahcuengh"),
    ("zh", "中文"),
    ("zu", "isiZulu"),
];
