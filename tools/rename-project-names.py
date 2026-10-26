#!/usr/bin/env python3
"""Coordinated nijigenerate/nijilive -> soul2d namespace, path and product
identity rename for this repository.

Apply is intentionally conservative:

* An explicit ordered move map renames directories and files. A collision
  at any destination aborts the run; nothing unrelated is overwritten.
* Text files are rewritten line-by-line. Lines carrying attribution,
  provenance or upstream-engine references are preserved verbatim, so
  copyright headers and historical URLs keep their original names.
* Every modified or moved file is snapshotted once into the ignored
  namespace-rename evidence directory before it is touched.

Usage:
    python3 tools/rename-project-names.py --check   # report pending changes
    python3 tools/rename-project-names.py --write   # apply the rename
    python3 tools/rename-project-names.py --selftest
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BACKUP_ROOT = REPO_ROOT / "build" / "verification" / "namespace-rename-20261011" / "originals"

# ---------------------------------------------------------------------------
# Scope

SKIP_DIRS = {
    ".git", ".dub", ".audit", "build", "out", "soul2d-live",
    "__pycache__",
}

# Only project-controlled roots are scanned; sibling checkouts, dependency
# caches and generated trees are never in scope.
SCAN_ROOTS = (
    "source", "rust", "res", "tools", "build-aux", "doc", "tl",
    ".github", ".cursor/skills",
)

# Owned root files eligible for content rewrite; no other root file is
# touched.
ROOT_FILES = {
    "AGENTS.md", "BUILD.md", "README.md", "THIRD_PARTY.md", "TRANSLATING.md",
    "dub.sdl", "genpot.sh", "gentl.sh", "mergeall.sh", "po_import.py",
    "translation-validator.py", "vcvars.ps1", "version.txt",
}

# Credentials and external local state are never rewritten.
CREDENTIAL_PATTERNS = (".env", ".env.*", "*.pem", "*.key", "*.p12", "*.pfx")

# External dependency cache paths keep their original locations; ordered
# longest-first so nested names are protected before the bare prefix.
EXTERNAL_PATHS = (
    "/opt/nijigenerate-deps/nijilive",
    "/opt/nijigenerate-deps/i2d-imgui",
    "/opt/nijigenerate-deps/",
)

PROTECTED_EXACT = {
    "LICENSE", "LICENSE.md", "LICENSE.rtf", "COPYING", "NOTICE",
    "CONTRIBUTORS.md", "CONTRUBUTORS.md", "CODE_OF_CONDUCT.md",
    "dub.selections.json",
    ".gitignore",
    "tools/rename-project-names.py",
    "tools/prepare-soul2d-live.py",
    "tools/check-project-artifacts.py",
    "nijigenerate-crashdump.log",
    "nijigenerate-depthbone-gpu.log",
}

PROTECTED_GLOBS = (
    "doc/licenses/*",
    "doc/adr/*",
    "doc/handoff-*.md",
    "doc/branch-*.md",
    "doc/*review*.md",
    "doc/license-*.md",
    "doc/development-goal.md",
    "doc/vendor-comparison-contract.md",
    "doc/cubism-53-research.md",
    "doc/official-sample-verification.md",
    "doc/namespace-rename-*.md",
    "doc/content-rights-*.md",
)

# File classes decide how a bare `nijilive` token is rewritten:
#   code    -> soul2d_live   (module namespace)
#   pathish -> soul2d-live   (directory / package name)
#   prose   -> Soul2DLive    (display name in documentation)
CODE_EXTS = {".d", ".di", ".patch", ".rs", ".diff"}
PATHISH_EXTS = {".sh", ".yml", ".yaml", ".sdl", ".wxs", ".wixproj", ".sln",
                ".rc", ".desktop", ".xml", ".plist", ".cfg", ".toml"}

# ---------------------------------------------------------------------------
# Filesystem moves (source -> destination), applied in order.

MOVE_COMPONENT_RULES = (
    (re.compile(r"^nijigenerate_tests$"), "soul2d_tests", {"source"}),
    (re.compile(r"^nijigenerate$"), "soul2d", {"source"}),
    (re.compile(r"^nijilive$"), "soul2d_live", {"source"}),
    (re.compile(r"^nijigenerate(?=\.)"), "soul2d", {"build-aux"}),
    (re.compile(r"^nijilive-"), "soul2d-live-", {"build-aux"}),
    (re.compile(r"^io\.github\.nijigenerate\.nijigenerate\."),
     "io.github.geekjapan.soul2d.", {"build-aux"}),
    (re.compile(r"^verify-nijigenerate$"), "verify-soul2d", {".cursor"}),
)


def map_component(root_rel: Path, parent_rel: Path, name: str) -> str | None:
    """Return the renamed path component, or None if unchanged."""
    top = rel.parts[0] if (rel := root_rel).parts else ""
    for pat, rep, roots in MOVE_COMPONENT_RULES:
        if top not in roots:
            continue
        new = pat.sub(rep, name, count=1)
        if new != name:
            return new
    return None


def _scan_bases(root: Path):
    """Yield the directory roots that may be walked."""
    for rel in SCAN_ROOTS:
        base = root / rel
        if base.is_dir() and not base.is_symlink():
            yield base


def planned_moves(root: Path) -> list[tuple[Path, Path]]:
    """Collect rename moves, returning only the outermost entries."""
    moves: list[tuple[Path, Path]] = []
    for scan_root in _scan_bases(root):
        for base, dirs, files in os.walk(scan_root):
            base_rel = Path(base).relative_to(root)
            if any(part in SKIP_DIRS for part in base_rel.parts):
                dirs[:] = []
                continue
            if any(str(base_rel).startswith(str(m)) for m, _ in moves):
                dirs[:] = []
                continue
            for name in list(dirs) + files:
                parent_rel = base_rel / name
                new = map_component(parent_rel, base_rel, name)
                if new is not None:
                    moves.append((parent_rel, parent_rel.parent / new))
                    if name in dirs:
                        dirs.remove(name)
    return sorted(moves, key=lambda m: str(m[0]))


# ---------------------------------------------------------------------------
# Content rewrite

ENGINE_URL_PATH = "nijigenerate/nijilive"
ENGINE_URL_SENTINEL = "__ENGINE_URL_SENTINEL__"
APP_URL_PATH = "nijigenerate/nijigenerate"
APP_URL_SENTINEL = "__APP_URL_SENTINEL__"
ORG_URL = "github.com/nijigenerate"
ORG_SENTINEL = "__ORG_URL_SENTINEL__"

# Lines matching this regex are attribution/provenance records and keep
# their historical names verbatim.
PROVENANCE_LINE = re.compile(
    r"copyright|©|\(c\)\s*\d|authors?\b|spdx[- ]|attribution|\bupstream\b|"
    r"derived from|based on|fork(ed)? (of|from)|provenance|inherited|"
    r"inochi2d|original author",
    re.IGNORECASE,
)

# Ordered literal rewrites applied before token rules.
LITERAL_RULES = (
    ("source/nijigenerate_tests", "source/soul2d_tests"),
    ("source/nijigenerate", "source/soul2d"),
    ("source/nijilive", "source/soul2d_live"),
    ("io.github.nijigenerate.nijigenerate", "io.github.geekjapan.soul2d"),
    ("com.github.nijigenerate.nijigenerate.ResourcePanel",
     "io.github.geekjapan.soul2d.ResourcePanel"),
    ("nijigenerate.nijigenerate.project", "io.github.geekjapan.soul2d.project"),
    ("nijigenerate.nijigenerate", "io.github.geekjapan.soul2d"),
    ('Manufacturer="nijigenerate Project"',
     'Manufacturer="Soul2D contributors"'),
    ('VER_COMPANYNAME_STR         "nijigenerate Project"',
     'VER_COMPANYNAME_STR         "Soul2D contributors"'),
    ('VER_FILEDESCRIPTION_STR     "nijigenerate"',
     'VER_FILEDESCRIPTION_STR     "Soul2D"'),
    ('VER_INTERNALNAME_STR        "nijigenerate"',
     'VER_INTERNALNAME_STR        "Soul2D"'),
    ('VER_PRODUCTNAME_STR         "nijigenerate"',
     'VER_PRODUCTNAME_STR         "Soul2D"'),
    ("nijigenerate Project", "Soul2D Project"),
    ("<id>nijigenerate</id>", "<id>soul2d</id>"),
    ('dependency "nijilive" version="~>0.0.1"',
     'dependency "soul2d-live" path="soul2d-live"'),
)

TOKEN_RULES = (
    ("nijigenerate_tests", "soul2d_tests"),
    ("NIJIGENERATE", "SOUL2D"),
    ("Nijigenerate", "Soul2D"),
    ("NIJIGEN", "SOUL2D"),
    ("Nijilive", "Soul2DLive"),
    ("NIJILIVE", "SOUL2D_LIVE"),
)

NIJIGENERATE_PATHISH = re.compile(r"nijigenerate(?=[./-])")
NIJILIVE_MODULE = re.compile(r"nijilive(?=[._])")
NIJILIVE_SENTENCE = re.compile(r"\bnijilive(?=\s+[a-z])")
NIJILIVE_PACKAGE_TAIL = re.compile(r"\bnijilive(?=[-/'\"`])")
NIJILIVE_PACKAGE_HEAD = re.compile(r"(?<=[/\\'\"`=(])nijilive\b")
NIJILIVE_BARE = re.compile(r"\bnijilive\b")


def _file_class(rel: Path) -> str:
    if rel.name.endswith((".appdata.xml", ".metainfo.xml")):
        return "prosexml"
    ext = rel.suffix.lower()
    if ext in CODE_EXTS:
        return "code"
    if ext in PATHISH_EXTS:
        return "pathish"
    return "prose"


def _is_packaging(rel: Path) -> bool:
    return rel.parts and rel.parts[0] in ("build-aux", ".github")


def transform_line(line: str, cls: str, packaging: bool) -> str:
    if PROVENANCE_LINE.search(line):
        return line
    if APP_URL_PATH in line and not packaging:
        return line
    if ORG_URL in line and APP_URL_PATH not in line and not packaging:
        return line

    line = line.replace(ENGINE_URL_PATH, ENGINE_URL_SENTINEL)
    for ext_path in EXTERNAL_PATHS:
        line = line.replace(ext_path, f"__EXTERNAL_{EXTERNAL_PATHS.index(ext_path)}__")
    if packaging:
        line = line.replace(APP_URL_PATH, "geekjapan/Soul2D")
        line = line.replace("docs.github.com/nijigenerate",
                            "github.com/geekjapan/Soul2D")
        line = line.replace(ORG_URL, "github.com/geekjapan")
    else:
        line = line.replace(APP_URL_PATH, APP_URL_SENTINEL)
        line = line.replace(ORG_URL, ORG_SENTINEL)

    for old, new in LITERAL_RULES:
        line = line.replace(old, new)
    for old, new in TOKEN_RULES:
        line = line.replace(old, new)

    if cls == "code":
        line = line.replace("nijigenerate", "soul2d")
        line = line.replace("nijilive", "soul2d_live")
    elif cls == "prosexml":
        line = NIJIGENERATE_PATHISH.sub("soul2d", line)
        line = line.replace("nijigenerate", "Soul2D")
        line = NIJILIVE_MODULE.sub("soul2d_live", line)
        line = NIJILIVE_PACKAGE_TAIL.sub("soul2d-live", line)
        line = NIJILIVE_PACKAGE_HEAD.sub("soul2d-live", line)
        line = NIJILIVE_BARE.sub("Soul2DLive", line)
    elif cls == "pathish":
        line = line.replace("nijigenerate", "soul2d")
        line = NIJILIVE_MODULE.sub("soul2d_live", line)
        line = NIJILIVE_SENTENCE.sub("Soul2DLive", line)
        line = NIJILIVE_PACKAGE_TAIL.sub("soul2d-live", line)
        line = NIJILIVE_PACKAGE_HEAD.sub("soul2d-live", line)
        line = NIJILIVE_BARE.sub("soul2d-live", line)
    else:
        line = line.replace("nijigenerate", "soul2d")
        line = NIJILIVE_MODULE.sub("soul2d_live", line)
        line = NIJILIVE_PACKAGE_TAIL.sub("soul2d-live", line)
        line = NIJILIVE_PACKAGE_HEAD.sub("soul2d-live", line)
        line = NIJILIVE_BARE.sub("Soul2DLive", line)

    for i, ext_path in enumerate(EXTERNAL_PATHS):
        line = line.replace(f"__EXTERNAL_{i}__", ext_path)
    return (line.replace(ENGINE_URL_SENTINEL, ENGINE_URL_PATH)
                .replace(APP_URL_SENTINEL, APP_URL_PATH)
                .replace(ORG_SENTINEL, ORG_URL))


def transform_file(rel: Path, data: bytes) -> bytes | None:
    orig = data
    bom = ""
    if data.startswith(b"\xef\xbb\xbf"):
        data = data[3:]
        bom = "\ufeff"
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        return None
    cls = _file_class(rel)
    packaging = _is_packaging(rel)
    out = "".join(
        transform_line(line, cls, packaging)
        for line in text.splitlines(keepends=True)
    )
    result = (bom + out).encode("utf-8")
    return result if result != orig else None


def is_protected(rel: Path) -> bool:
    if rel.as_posix() in PROTECTED_EXACT or rel.name in PROTECTED_EXACT:
        return True
    if any(rel.match(g) for g in CREDENTIAL_PATTERNS):
        return True
    posix = rel.as_posix()
    return any(rel.match(g) or posix == g.rstrip("*") for g in PROTECTED_GLOBS)


def iter_target_files(root: Path, backup_root: Path | None = None):
    backup_rel = None
    backup = backup_root if backup_root is not None else BACKUP_ROOT
    try:
        backup_rel = backup.resolve().relative_to(root.resolve())
    except (ValueError, OSError):
        pass
    for name in sorted(ROOT_FILES):
        rel = Path(name)
        if (root / rel).is_file() and not is_protected(rel):
            yield rel
    for scan_root in _scan_bases(root):
        for base, dirs, files in os.walk(scan_root):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            base_rel = Path(base).relative_to(root)
            if backup_rel is not None:
                dirs[:] = [d for d in dirs
                           if not str((base_rel / d).as_posix()).startswith(
                               str(backup_rel))]
            for name in sorted(files):
                rel = base_rel / name
                if is_protected(rel):
                    continue
                yield rel


# ---------------------------------------------------------------------------
# Apply / check

def _assert_safe_parents(root: Path, rel: Path) -> Path:
    """Refuse symlinked roots/ancestors and escapes; root itself is checked
    before resolve() can hide a symlink."""
    if root.is_symlink():
        raise ValueError(f"refusing symlinked root: {root}")
    resolved_root = root.resolve(strict=False)
    target = (root / rel).resolve(strict=False)
    if os.path.commonpath([str(resolved_root), str(target)]) != str(resolved_root):
        raise ValueError(f"path escapes root: {rel}")
    probe = root
    for part in rel.parts[:-1]:
        probe = probe / part
        if probe.is_symlink():
            raise ValueError(f"refusing symlinked ancestor: {probe}")
    return root / rel


def _safe_under(root: Path, rel: Path) -> Path:
    return _assert_safe_parents(root, rel)


def _ensure_backup(root: Path, rel: Path, backup_root: Path) -> None:
    dst = _assert_safe_parents(backup_root, rel)
    if dst.exists():
        return
    src = _assert_safe_parents(root, rel)
    if not src.is_file():
        return
    _assert_safe_parents(backup_root, rel.parent)
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)


def compute_changes(root: Path, backup_root: Path | None = None):
    """Return (edits, moves): files whose content differs and pending moves."""
    edits: list[tuple[Path, bytes]] = []
    for rel in iter_target_files(root, backup_root):
        p = root / rel
        if p.is_symlink() or not p.is_file():
            continue
        data = p.read_bytes()
        new = transform_file(rel, data)
        if new is not None:
            edits.append((rel, new))
    moves = [(s, d) for s, d in planned_moves(root) if (root / s).exists()]
    collisions = [d for _, d in moves if (root / d).exists()]
    missing = [(s, d) for s, d in planned_moves(root)
               if not (root / s).exists() and not (root / d).exists()]
    return edits, moves, collisions, missing


def apply(root: Path, backup_root: Path | None = None) -> int:
    backup_root = backup_root if backup_root is not None else BACKUP_ROOT
    edits, moves, collisions, missing = compute_changes(root, backup_root)
    if collisions:
        for c in collisions:
            print(f"error: move destination already exists: {c}",
                  file=sys.stderr)
        return 2
    for s, d in missing:
        print(f"warning: neither {s} nor {d} exists", file=sys.stderr)
    try:
        # Preflight: validate every write/move/backup path before editing.
        for rel, _ in edits:
            _assert_safe_parents(root, rel)
            _assert_safe_parents(backup_root, rel)
        for src, dst in moves:
            _assert_safe_parents(root, src)
            _assert_safe_parents(root, dst.parent)
            if (root / src).is_dir():
                for p in sorted((root / src).rglob("*")):
                    if p.is_file():
                        _assert_safe_parents(backup_root,
                                             p.relative_to(root))
            else:
                _assert_safe_parents(backup_root, src)
    except ValueError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    for rel, new in edits:
        _ensure_backup(root, rel, backup_root)
        (root / rel).write_bytes(new)
    for src, dst in moves:
        if (root / src).is_dir():
            for p in sorted((root / src).rglob("*")):
                if p.is_file():
                    _ensure_backup(root, p.relative_to(root), backup_root)
        else:
            _ensure_backup(root, src, backup_root)
        (root / dst).parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(root / src), str(root / dst))
    print(f"renamed {len(moves)} paths, rewrote {len(edits)} files")
    return 0


def check(root: Path) -> int:
    edits, moves, collisions, missing = compute_changes(root)
    problems = 0
    for rel, _ in edits:
        print(f"pending edit: {rel}")
        problems += 1
    for s, d in moves:
        print(f"pending move: {s} -> {d}")
        problems += 1
    for c in collisions:
        print(f"collision: {c}")
        problems += 1
    for s, d in missing:
        print(f"warning: neither {s} nor {d} exists")
    if problems == 0:
        print("check: rename fully applied, no pending changes")
        return 0
    print(f"check: {problems} pending change(s)")
    return 1


# ---------------------------------------------------------------------------

def selftest() -> int:
    root = Path(tempfile.mkdtemp(prefix="rename-selftest-"))
    backups = Path(tempfile.mkdtemp(prefix="rename-selftest-backups-"))
    try:
        (root / "source" / "nijigenerate" / "api").mkdir(parents=True)
        (root / "source" / "nijilive").mkdir(parents=True)
        (root / "source" / "nijigenerate_tests").mkdir(parents=True)
        (root / "build-aux" / "patches").mkdir(parents=True)
        # External look-alike trees and credentials are never in scope.
        (root / "nijilive").mkdir()
        (root / "nijilive" / "ext.d").write_text("module nijilive.ext;\n")
        (root / ".env").write_text("SECRET=nijigenerate\n")
        (root / "source" / "cred.pem").write_text("nijigenerate-key\n")

        app_d = (root / "source" / "nijigenerate" / "api" / "mcp.d")
        app_d.write_text(
            "// Copyright 2024, Inochi2D Project\n"
            "// Upstream: https://github.com/nijigenerate/nijilive\n"
            "module nijigenerate.api.mcp;\n"
            'enum URI = "resource://nijigenerate/project";\n'
            "import nijilive.core;\n"
            'enum NAME = "Nijigenerate MCP";\n'
            'enum KEY = "com.github.nijigenerate.nijigenerate.ResourcePanel";\n'
        )
        (root / "source" / "nijilive" / "core.d").write_text(
            "module nijilive.core;\n")
        (root / "source" / "nijilive" / "blob.bin").write_bytes(
            bytes([0x89, 0x00, 0xff, 0x01]))
        (root / "source" / "nijigenerate_tests" / "t.d").write_text(
            "module nijigenerate_tests.t;\nimport nijigenerate.api.mcp;\n")
        patch = root / "build-aux" / "patches" / "nijilive-fix.patch"
        patch.write_text(
            "--- a/source/nijilive/x.d\n+++ b/source/nijilive/x.d\n")
        plist = root / "build-aux" / "osx.plist"
        plist.write_text("<string>io.github.nijigenerate.nijigenerate</string>\n")

        rc = apply(root, backup_root=backups)
        moved = root / "source" / "soul2d" / "api" / "mcp.d"
        checks = [
            rc == 0,
            moved.is_file(),
            "module soul2d.api.mcp;" in moved.read_text(),
            '"resource://soul2d/project"' in moved.read_text(),
            "import soul2d_live.core;" in moved.read_text(),
            '"Soul2D MCP"' in moved.read_text(),
            '"io.github.geekjapan.soul2d.ResourcePanel"'
                in moved.read_text(),
            "com.github.io.github" not in moved.read_text(),
            "Copyright 2024, Inochi2D Project" in moved.read_text(),
            "github.com/nijigenerate/nijilive" in moved.read_text(),
            (root / "source" / "soul2d_live" / "core.d").is_file(),
            "module soul2d_live.core;" in
                (root / "source" / "soul2d_live" / "core.d").read_text(),
            (root / "source" / "soul2d_live" / "blob.bin").read_bytes()
                == bytes([0x89, 0x00, 0xff, 0x01]),
            (backups / "source" / "nijilive" / "blob.bin").read_bytes()
                == bytes([0x89, 0x00, 0xff, 0x01]),
            (root / "source" / "soul2d_tests" / "t.d").is_file(),
            "module soul2d_tests.t;" in
                (root / "source" / "soul2d_tests" / "t.d").read_text(),
            (root / "build-aux" / "patches" / "soul2d-live-fix.patch").is_file(),
            "soul2d_live/x.d" in
                (root / "build-aux" / "patches" / "soul2d-live-fix.patch").read_text(),
            "io.github.geekjapan.soul2d" in plist.read_text(),
            check(root) == 0,
            (backups / "source" / "nijigenerate" / "api" / "mcp.d").is_file(),
            # Out-of-scope content is byte-identical.
            (root / "nijilive" / "ext.d").read_text()
                == "module nijilive.ext;\n",
            (root / ".env").read_text() == "SECRET=nijigenerate\n",
            (root / "source" / "cred.pem").read_text() == "nijigenerate-key\n",
        ]

        # Idempotent second run.
        rc2 = apply(root, backup_root=backups)
        checks.append(rc2 == 0)
        checks.append(check(root) == 0)

        # Collision refusal.
        other = Path(tempfile.mkdtemp(prefix="rename-collision-"))
        try:
            (other / "source" / "nijilive").mkdir(parents=True)
            (other / "source" / "soul2d_live").mkdir(parents=True)
            (other / "source" / "nijilive" / "x.d").write_text("module nijilive.x;\n")
            (other / "source" / "soul2d_live" / "x.d").write_text("module soul2d_live.x;\n")
            checks.append(apply(other, backup_root=backups) == 2)
            checks.append(
                (other / "source" / "soul2d_live" / "x.d").read_text()
                == "module soul2d_live.x;\n")
        finally:
            shutil.rmtree(other, ignore_errors=True)

        # Symlinked backup root is refused before any write.
        linked = Path(tempfile.mkdtemp(prefix="rename-link-"))
        try:
            link_root = linked / "linkbackups"
            link_root.symlink_to(backups)
            (linked / "source" / "nijilive").mkdir(parents=True)
            (linked / "source" / "nijilive" / "x.d").write_text(
                "module nijilive.x;\n")
            checks.append(apply(linked, backup_root=link_root) == 2)
            checks.append((linked / "source" / "nijilive" / "x.d").read_text()
                          == "module nijilive.x;\n")
        finally:
            shutil.rmtree(linked, ignore_errors=True)

        if all(checks):
            print("selftest: namespace imports, URIs, identifiers, "
                  "provenance preservation, scope/credential exclusion, "
                  "backup safety, collision refusal and idempotence verified")
            return 0
        for i, ok in enumerate(checks):
            if not ok:
                print(f"selftest check {i} failed", file=sys.stderr)
        return 1
    finally:
        shutil.rmtree(root, ignore_errors=True)
        shutil.rmtree(backups, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--check", action="store_true")
    g.add_argument("--write", action="store_true")
    g.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    if args.write:
        return apply(REPO_ROOT)
    return check(REPO_ROOT)


if __name__ == "__main__":
    sys.exit(main())
