# ADR 0004: Do not write `.moc3`, and stop the runtime export at the files a CMO3 holds

- **Status:** Accepted on 2026-10-18 by the maintainer's explicit decision (issue #234), as `doc/acceptance-scope.md` requires for a scope exclusion
- **Date:** 2026-10-18
- **Context:** Ledger row EXP08 asks for the runtime data a Cubism Editor export writes: `.moc3`, `.model3.json`, `.cdi3.json`, `.physics3.json`, `.pose3.json` and `.exp3.json`. Soul2D writes `.cdi3.json` (PR #336) and `.physics3.json` (PR #337, `live2d.physics3.export`). The `.moc3` format is binary, undocumented by the vendor, and its use is bound by the vendor's licence terms. No element for a pose or an expression was found in the 94 sample CMO3 files, so a CMO3 alone does not hold the content of `.pose3.json` or `.exp3.json`.

## Decision

1. Soul2D does not write `.moc3`. No reverse-engineered writer is started.
2. Soul2D does not write `.model3.json`. It names a `.moc3`, so a runtime cannot use it without one.
3. `.pose3.json` and `.exp3.json` are not written from a CMO3, because the CMO3 does not hold their content.
4. The runtime export of row EXP08 is the `.cdi3.json` and `.physics3.json` writers that exist. Vendor acceptance of both stays NOT_RUN.
5. Reading is allowed (maintainer decision, 2026-10-18, issue #228). Soul2D may read `.moc3` and `.model3.json` with
   its own clean-room reader built from the structure of local sample files, with no new dependency and no vendor
   source. The reader refuses a version or an element it does not understand instead of guessing. This changes
   nothing in points 1 to 3.

## Revisit when

- the vendor publishes the `.moc3` format with terms that allow a third-party writer, or
- a documented source of pose and expression data inside Cubism project files is found.
