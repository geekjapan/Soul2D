# History, undo and redo

Status: run live against the headless host (`undo`, `redo`, `history_summary`, `history_goto` with a stale and a current revision, `history_limit_set`, `history_clear`).

One shared history for INP/INX and native documents. Every persistent edit is one entry with a label (`node.create`, `merge.images`, `binding.set_trs`, ...). Preview calls
(`value_set`, `animation_seek`, `physics_step`, renders, exports) add none.

## Sequence

1. Read: `history_summary {}` answers `{position, count, limit, revision, states:[{index, label, current, applied}]}`.
2. Act: `undo {}` / `redo {}` step one entry. `history_goto {index, expectedRevision}` jumps to a retained state; read `revision` immediately before and pass it.
3. Confirm: `history_summary` again (`position`, `applied` flags), then re-read the resource that changed. Undo and redo answer a full snapshot.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `undo` / `EditCommand_Undo` | `{}` fails with "nothing to undo" at the start. |
| `redo` / `EditCommand_Redo` | `{}` fails with "nothing to redo"; any new edit drops the redo states. |
| `history_summary` | `{}` read only. |
| `history_goto` | `{index, expectedRevision}` both required. |
| `history_limit_set` | `{limit}` 2 to 1000, default 100. Shrinking drops the oldest states first, never the current one. |
| `history_clear` | `{}` keep only the current state; its label is emptied. |

## Gotchas

- Verified: `history_goto` with a wrong `expectedRevision` fails with "history.goto expected revision 1, but the session is at 95" and changes nothing; with the right one it
  moved `position` and `revision` rose from 95 to 96. Without `expectedRevision` it fails with "requires an integer expectedRevision".
- Verified: `undo` raises `revision` (6 to 7 in one run); revision counts changes of state, not entries, so it does not go down with undo.
- Verified: `history_limit_set {limit:2}` after several edits left two states and raised `revision` (97); `history_clear` raised it again (98).
- Verified: `new`, `import_*` and `open` reset history to a single empty state but `revision` keeps counting up.
- Verified: a call that fails leaves history and `revision` as they were.
- Verified: an edit that does not change the document (setting a property to its current value) adds no entry.
- The editor undo menu uses this same history; an edit made there between your read and your write moves `revision` (see SKILL.md, revision check).
