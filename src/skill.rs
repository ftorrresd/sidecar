//! `sidecar skill` — dump a Claude-Code-style SKILL.md into a temp directory.
//!
//! The printed directory can be installed as a skill so a coding agent learns
//! the on-disk format of sidecar's review notes (`.sidecar/*.json`) and can read,
//! act on, and resolve them directly from the repository.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// The skill document taught to coding agents. Kept in sync with `notes.rs`.
const SKILL_MD: &str = r##"---
name: sidecar-notes
description: >-
  Read and resolve sidecar review notes: anchored code-review comments a
  reviewer left on specific files/lines, stored as JSON under a repo's
  `.sidecar/` directory. Use whenever the user refers to "sidecar notes", review
  comments, or asks you to address feedback pinned to code.
---

# Sidecar review notes

`sidecar` is a diff/review TUI. While reviewing, a person selects a range of a
file and attaches a **note** (a comment). Notes are saved in the repository so an
agent can read them, act on them, and resolve them.

Use this skill to: find outstanding review notes, map each to its exact code
location, make the requested change, and clear the note when done.

## Where notes live

```
<repo-root>/.sidecar/<encoded-path>.json     # one file per annotated source file
```

The filename is the repo-relative path with every character outside
`[A-Za-z0-9._-]` percent-encoded — e.g. notes for `src/app.rs` live in
`.sidecar/src%2Fapp.rs.json`. The real path is also stored in the JSON, so prefer
the `path` field over decoding the filename.

To list every note in a repo, read all `.sidecar/*.json` files. If the directory
does not exist, there are no notes.

## File format

```json
{
  "path": "src/app.rs",
  "notes": [
    {
      "id": 1783960708990456775,
      "kind": "char",
      "start_line": 42,
      "start_col": 8,
      "end_line": 42,
      "end_col": 20,
      "text": "This clone looks unnecessary - can we borrow instead?",
      "snippet": ["    let name = user.name.clone();"],
      "created_at": "2026-07-13T18:24:50Z"
    }
  ]
}
```

### Fields

- `path` — repo-relative file the notes belong to.
- `id` — unique note id (creation time in nanoseconds).
- `kind` — `"char"` (a character range) or `"line"` (whole lines).
- `start_line` / `end_line` — **1-based**, inclusive.
- `start_col` / `end_col` — **0-based**, inclusive character offsets into the
  line **after tabs are expanded to 4 spaces**. Unused for `kind: "line"`.
- `text` — the reviewer's comment. May contain newlines (multi-line note).
- `snippet` — the exact selected text, line by line, captured when the note was
  made. Use it to relocate the target if line numbers have shifted.
- `created_at` — ISO-8601 UTC timestamp.

### Interpreting a selection

- `kind: "line"` covers every line from `start_line` to `end_line` inclusive;
  columns are irrelevant.
- `kind: "char"` covers `(start_line, start_col)` .. `(end_line, end_col)`
  inclusive, in reading order. Single-line selections have
  `start_line == end_line`.

Columns count characters on the **tab-expanded** line (each tab = 4 spaces); if
the source uses tabs, expand them before indexing by column.

## Workflow

1. **Collect** — read `<repo>/.sidecar/*.json`.
2. **Locate** — open each note's `path` and go to the range. If the lines no
   longer match (the file changed), search for `snippet`; the note refers to that
   text wherever it now lives.
3. **Act** — treat `text` as an instruction or question from the reviewer, and
   make the requested change (or answer it).
4. **Resolve** — once addressed, remove that note object from the `notes` array.
   If a file's `notes` becomes empty, delete its `.sidecar/<encoded-path>.json`.

## Staying consistent with sidecar

- sidecar re-validates notes on every refresh: if a note's `snippet` still exists
  (possibly moved) it re-anchors the line numbers; if the snippet is gone it
  drops the note. So you need not update `start_line` after edits — either
  resolve (delete) the note or leave it and let sidecar re-anchor. Do not
  hand-edit `snippet`.
- Preserve the JSON shape (same fields) so sidecar can keep reading the file.
"##;

/// Write `SKILL.md` into a temp directory and return that directory's path.
pub fn dump() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("sidecar-notes-skill");
    std::fs::create_dir_all(&dir).context("creating the skill directory")?;
    std::fs::write(dir.join("SKILL.md"), SKILL_MD).context("writing SKILL.md")?;
    Ok(dir)
}
