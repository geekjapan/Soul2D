#!/usr/bin/env python3
"""Check the English/Japanese documentation twins and their links.

Fails when a page under user-doc/tutorial/en has no twin under user-doc/tutorial/ja (or the reverse),
when a twin does not start with a link to the other language, or when a relative Markdown link in the
README files, doc/README.md or user-doc/ points at a missing file.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TUTORIAL = os.path.join(ROOT, "user-doc", "tutorial")
LINK = re.compile(r"\]\(([^)\s]+)\)")


def pages(lang):
    base = os.path.join(TUTORIAL, lang)
    return {os.path.relpath(os.path.join(d, f), base)
            for d, _, files in os.walk(base) for f in files if f.endswith(".md")}


def first_line(path):
    with open(path, encoding="utf-8") as f:
        return f.readline()


def twin_link(page, src, dst):
    # Link from user-doc/tutorial/<src>/<page> to user-doc/tutorial/<dst>/<page>.
    here = os.path.dirname(os.path.join(TUTORIAL, src, page))
    return os.path.relpath(os.path.join(TUTORIAL, dst, page), here)


def broken_links(path):
    with open(path, encoding="utf-8") as f:
        text = re.sub(r"```.*?```", "", f.read(), flags=re.S)
    for target in LINK.findall(text):
        if re.match(r"[a-z]+:", target) or target.startswith("#"):
            continue
        file_part = target.split("#", 1)[0]
        if not os.path.exists(os.path.normpath(os.path.join(os.path.dirname(path), file_part))):
            yield target


def main():
    errors = []
    en, ja = pages("en"), pages("ja")
    errors += [f"user-doc/tutorial/en/{p}: no Japanese twin" for p in sorted(en - ja)]
    errors += [f"user-doc/tutorial/ja/{p}: no English twin" for p in sorted(ja - en)]
    for page in sorted(en & ja):
        for src, dst in (("en", "ja"), ("ja", "en")):
            want = twin_link(page, src, dst)
            if f"]({want})" not in first_line(os.path.join(TUTORIAL, src, page)):
                errors.append(f"user-doc/tutorial/{src}/{page}: first line does not link to {want}")
    for name, other in (("README.md", "README.ja.md"), ("README.ja.md", "README.md")):
        with open(os.path.join(ROOT, name), encoding="utf-8") as f:
            head = f.read(400)
        if f"]({other})" not in head:
            errors.append(f"{name}: no link to {other} at the top")

    checked = [os.path.join(ROOT, n) for n in ("README.md", "README.ja.md", os.path.join("doc", "README.md"))]
    checked += [os.path.join(d, f) for d, _, files in os.walk(os.path.join(ROOT, "user-doc"))
                for f in files if f.endswith(".md")]
    for path in checked:
        errors += [f"{os.path.relpath(path, ROOT)}: broken link {t}" for t in broken_links(path)]

    for e in errors:
        print(e, file=sys.stderr)
    print(f"{len(en)} tutorial pages, {len(checked)} files link-checked, {len(errors)} errors")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
