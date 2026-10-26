# Translating Soul2D

Soul2D uses gettext for translations. Plural and context forms are not supported: the editor looks up each
string by its exact msgid and shows the msgid when no translation exists.
You need a gettext distribution (`msginit`, `msgmerge`, `msgfmt`) to work on translation files.
Language support is limited, so some scripts may render incorrectly. The editor has no right-to-left text
layout, so right-to-left languages are not supported.

## Create a translation file for a new language

To create a new translation file, run:
```sh
msginit --locale=<langcode> --input=tl/template.pot -o tl/<langcode>.po
```
Replace `<langcode>` with your language code.

Set the charset in the new `.po` file to UTF-8. Soul2D supports only UTF-8.

## Merge the latest template

```sh
msgmerge -o tl/<langcode>_merged.po tl/<langcode>.po tl/template.pot
```

Check that the merged entries make sense, then replace `<langcode>.po` with `<langcode>_merged.po`.

## Validate your translation

Translations can contain format placeholders and Material icon symbols. The validator checks that a translation keeps both:
```sh
# install dependencies
pip install babel
# validate
python translation-validator.py -f tl/<langcode>.po
# or validate all
python translation-validator.py -a
```

### Check zh-TW locale consistency

Uses OpenCC to flag entries that may not follow zh-TW conventions.
```sh
# additional dependency for this check only
pip install opencc-python-reimplemented

python translation-validator.py -t -f tl/zh-TW.po

# Stricter threshold (only show near-zero change entries)
python translation-validator.py -t -f tl/zh-TW.po --trad-threshold 0.05
```

## Import translations from another .po file

`po_import.py` merges translations from a source `.po` into a target `.po` **without touching the target's formatting, comments, or whitespace**.

```sh
# Fill in only empty msgstr entries (safe, non-destructive)
python3 po_import.py SOURCE.po tl/<langcode>.po

# Write result to a new file instead of modifying the target in-place
python3 po_import.py SOURCE.po tl/<langcode>.po --out output.po

# Also overwrite non-empty msgstr entries that differ from the source
python3 po_import.py SOURCE.po tl/<langcode>.po --overwrite

# Include fuzzy entries from the source
python3 po_import.py SOURCE.po tl/<langcode>.po --use-fuzzy
```

> **Note:** Fuzzy source entries are skipped by default. The script never modifies comments, `#:` references, flags, or blank lines in the target file.

## Reorder format parameters

To choose which format parameter a placeholder uses, add the `<index>$` operator.
For example, `%2$s` uses the second argument of the format string.

## Generate output files

```sh
./gentl.sh                                   # every tl/*.po into out/*.mo
msgfmt tl/<langcode>.po -o <langcode>.mo     # a single language
```
The editor reads both `.mo` and `.po` files, and a `.mo` wins over a `.po` with the same language code.
Put the file in the `i18n` folder of the configuration directory, or next to the executable for testing.

## Translation storage

The configuration directory is `<config>/soul2d`, where `<config>` is
 * `$XDG_CONFIG_HOME` or `~/.config` on Linux
 * `~/Library/Application Support` on macOS
 * `%AppData%` on Windows (the folder is `.soul2d` there)

An existing `.soul2d` folder is used instead of `soul2d` on Linux and macOS, and
`INOCHI_CONFIG_PATH` overrides the whole path. Translations go in its `i18n` subfolder.
The editor also looks in the executable's directory, in `i18n` next to the executable on Windows,
in `../Resources/i18n` on macOS and in the AppImage root on Linux.

## Extract messages

`python3 tools/genpot.py` rewrites `tl/template.pot` from `rust/src/**/*.rs` (standard library only; the output is
sorted by file and line, with `#: rust/src/file.rs:LINE` locations). A string is translatable when it is the literal
argument of `tr("...")` (`editor_ui.rs`, `editor_shell.rs`) or `env.t("...")` (`editor_windows.rs`); the code has no
plural or context forms. Calls inside `#[cfg(test)]` items are ignored.

Strings that `editor_windows.rs` looks up through a table (`env.t(label)`) are listed in `tl/genpot-extra.txt`; the
script fails if one of them is no longer a literal in the sources. `python3 tools/genpot.py --dynamic` lists those call
sites, so check it when you add a table-driven label. `--check` fails when the template is stale and `--self-test`
runs the extractor on sample source.

The template holds only the strings the Rust code looks up, far fewer than the retired D template. After
regenerating it, bring a language up to date with the `msgmerge` command above; entries the code no longer uses
become obsolete (`#~`) there. `mergeall.sh` runs that merge over every `tl/*.po` in place, so review the diff first.
