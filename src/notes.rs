//! Review notes: anchored comments a reviewer attaches to a range of a file.
//!
//! A note pins to a character (`v`) or line (`V`) range of a file's *working-tree*
//! content. Notes live in `<repo>/.sidecar/<encoded-path>.json` (one file per
//! source file) so they persist between sessions and can be committed or ignored
//! as the user sees fit.
//!
//! Because the file keeps changing underneath the notes, each note also stores a
//! copy of the text it was taken over (`snippet`). On every refresh we
//! [`revalidate`] each note against the current content: if the snippet still sits
//! at the recorded coordinates we keep it as-is; if it has merely moved we
//! re-anchor it to the new line; if it is gone we drop it.
//!
//! Coordinate convention: `*_line` are 1-based; `*_col` are 0-based character
//! offsets into the *tab-expanded* line (see [`canon_lines`]), inclusive on both
//! ends. Columns are unused for line-wise notes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Tabs are expanded to this many spaces so that a character column maps 1:1 to
/// a terminal cell (and so notes anchor consistently regardless of tab display).
const TAB_WIDTH: usize = 4;

/// Whether a selection covers characters (`v`) or whole lines (`V`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SelKind {
    Char,
    Line,
}

/// A single anchored note.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Note {
    pub id: u64,
    pub kind: SelKind,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// The reviewer's comment.
    pub text: String,
    /// The selected text, line by line, used to re-anchor on refresh.
    pub snippet: Vec<String>,
    /// ISO-8601 UTC timestamp when the note was created, e.g. `2026-07-13T10:04:00Z`.
    pub created_at: String,
}

impl Note {
    /// Build a note from a selection over `lines` (already tab-expanded).
    ///
    /// Coordinates are normalized so start precedes end. Returns `None` if the
    /// selection does not resolve to any text.
    pub fn new(
        kind: SelKind,
        (mut sl, mut sc): (usize, usize),
        (mut el, mut ec): (usize, usize),
        text: String,
        lines: &[String],
    ) -> Option<Note> {
        // Normalize so (sl,sc) <= (el,ec) in reading order.
        if (el, ec) < (sl, sc) {
            std::mem::swap(&mut sl, &mut el);
            std::mem::swap(&mut sc, &mut ec);
        }
        let snippet = slice(kind, lines, sl, sc, el, ec)?;
        Some(Note {
            id: unique_id(),
            kind,
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
            text,
            snippet,
            created_at: now_iso8601(),
        })
    }

    /// Whether `(line, col)` (1-based line, 0-based col) falls inside this note.
    pub fn contains(&self, line: usize, col: usize) -> bool {
        match self.kind {
            SelKind::Line => line >= self.start_line && line <= self.end_line,
            SelKind::Char => {
                let after_start =
                    line > self.start_line || (line == self.start_line && col >= self.start_col);
                let before_end =
                    line < self.end_line || (line == self.end_line && col <= self.end_col);
                after_start && before_end
            }
        }
    }
}

/// The on-disk shape of one file's notes.
#[derive(Serialize, Deserialize, Default)]
struct FileNotes {
    path: String,
    notes: Vec<Note>,
}

/// The directory notes are stored under.
pub fn dir(root: &Path) -> PathBuf {
    root.join(".sidecar")
}

/// The JSON file backing notes for `rel` (a repo-relative path).
fn file_path(root: &Path, rel: &str) -> PathBuf {
    dir(root).join(format!("{}.json", encode_filename(rel)))
}

/// Load every file's notes from `.sidecar`, keyed by repo-relative path.
pub fn load_all(root: &Path) -> HashMap<String, Vec<Note>> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir(root)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(fnotes) = serde_json::from_str::<FileNotes>(&data) {
            if !fnotes.notes.is_empty() {
                out.insert(fnotes.path, fnotes.notes);
            }
        }
    }
    out
}

/// Persist `notes` for `rel`. Removes the backing file when there are none left.
pub fn save(root: &Path, rel: &str, notes: &[Note]) -> Result<()> {
    let path = file_path(root, rel);
    if notes.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        return Ok(());
    }
    std::fs::create_dir_all(dir(root)).context("creating .sidecar directory")?;
    let fnotes = FileNotes {
        path: rel.to_string(),
        notes: notes.to_vec(),
    };
    let json = serde_json::to_string_pretty(&fnotes).context("serializing notes")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Split file `content` into tab-expanded lines for consistent column mapping.
pub fn canon_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|l| l.replace('\t', &" ".repeat(TAB_WIDTH)))
        .collect()
}

/// Re-anchor `note` against the current `lines`. Returns `false` if the note's
/// text is gone and it should be dropped.
pub fn revalidate(note: &mut Note, lines: &[String]) -> bool {
    // Fast path: the snippet is still exactly where we left it.
    if slice(
        note.kind,
        lines,
        note.start_line,
        note.start_col,
        note.end_line,
        note.end_col,
    )
    .as_ref()
        == Some(&note.snippet)
    {
        return true;
    }
    // Otherwise try to find where the snippet moved to.
    match relocate(note.kind, lines, &note.snippet, note.start_line) {
        Some((sl, sc, el, ec)) => {
            note.start_line = sl;
            note.start_col = sc;
            note.end_line = el;
            note.end_col = ec;
            true
        }
        None => false,
    }
}

/// The text a selection covers, joined by newlines. Char columns are clamped to
/// real characters (the cursor may rest just past a line's end). Used to mirror
/// a live selection to the clipboard.
pub fn selection_text(
    kind: SelKind,
    a: (usize, usize),
    b: (usize, usize),
    lines: &[String],
) -> Option<String> {
    let (mut s, mut e) = (a, b);
    if e < s {
        std::mem::swap(&mut s, &mut e);
    }
    if kind == SelKind::Char {
        let clamp = |(l, c): (usize, usize)| -> (usize, usize) {
            let len = lines
                .get(l.saturating_sub(1))
                .map(|x| x.chars().count())
                .unwrap_or(0);
            (l, if len == 0 { 0 } else { c.min(len - 1) })
        };
        s = clamp(s);
        e = clamp(e);
    }
    slice(kind, lines, s.0, s.1, e.0, e.1).map(|v| v.join("\n"))
}

// ---- Text slicing / relocation -----------------------------------------------

/// Extract the text a selection covers, or `None` if the coordinates fall
/// outside `lines`. `sl`/`el` are 1-based; `sc`/`ec` are 0-based inclusive.
fn slice(
    kind: SelKind,
    lines: &[String],
    sl: usize,
    sc: usize,
    el: usize,
    ec: usize,
) -> Option<Vec<String>> {
    if sl == 0 || el < sl || el > lines.len() {
        return None;
    }
    let (s0, e0) = (sl - 1, el - 1);
    match kind {
        SelKind::Line => Some(lines[s0..=e0].to_vec()),
        SelKind::Char => {
            if s0 == e0 {
                let chars: Vec<char> = lines[s0].chars().collect();
                if chars.is_empty() {
                    return (sc == 0 && ec == 0).then(|| vec![String::new()]);
                }
                if sc > ec || ec >= chars.len() {
                    return None;
                }
                Some(vec![chars[sc..=ec].iter().collect()])
            } else {
                let first: Vec<char> = lines[s0].chars().collect();
                if sc > first.len() {
                    return None;
                }
                let mut out = vec![first[sc..].iter().collect()];
                out.extend(lines[s0 + 1..e0].iter().cloned());
                let last: Vec<char> = lines[e0].chars().collect();
                if last.is_empty() {
                    if ec != 0 {
                        return None;
                    }
                    out.push(String::new());
                } else {
                    if ec >= last.len() {
                        return None;
                    }
                    out.push(last[..=ec].iter().collect());
                }
                Some(out)
            }
        }
    }
}

/// Search `lines` for `snippet`, returning the (start_line, start_col, end_line,
/// end_col) of the occurrence nearest `hint_line`. 1-based lines.
fn relocate(
    kind: SelKind,
    lines: &[String],
    snippet: &[String],
    hint_line: usize,
) -> Option<(usize, usize, usize, usize)> {
    let n = snippet.len();
    if n == 0 || n > lines.len() {
        return None;
    }
    let mut best: Option<(usize, usize, usize, usize)> = None;
    let mut best_dist = usize::MAX;
    for s0 in 0..=lines.len() - n {
        if let Some((sc, ec)) = block_matches(kind, lines, s0, snippet) {
            let sl = s0 + 1;
            let dist = sl.abs_diff(hint_line);
            if dist < best_dist {
                best_dist = dist;
                best = Some((sl, sc, s0 + n, ec));
            }
        }
    }
    best
}

/// Whether `snippet` matches the block of `lines` starting at `s0` (0-based).
/// On a match, returns the (start_col, end_col) of the covered characters.
fn block_matches(
    kind: SelKind,
    lines: &[String],
    s0: usize,
    snippet: &[String],
) -> Option<(usize, usize)> {
    let n = snippet.len();
    match kind {
        SelKind::Line => {
            for i in 0..n {
                if lines[s0 + i] != snippet[i] {
                    return None;
                }
            }
            Some((0, 0))
        }
        SelKind::Char if n == 1 => {
            let hay: Vec<char> = lines[s0].chars().collect();
            let needle: Vec<char> = snippet[0].chars().collect();
            if needle.is_empty() {
                return hay.is_empty().then_some((0, 0));
            }
            let sc = find_subslice(&hay, &needle)?;
            Some((sc, sc + needle.len() - 1))
        }
        SelKind::Char => {
            // First line: snippet[0] must be a suffix; last: a prefix; middle: exact.
            let first: Vec<char> = lines[s0].chars().collect();
            let first_needle: Vec<char> = snippet[0].chars().collect();
            if first_needle.len() > first.len()
                || first[first.len() - first_needle.len()..] != first_needle[..]
            {
                return None;
            }
            let sc = first.len() - first_needle.len();
            for i in 1..n - 1 {
                if lines[s0 + i] != snippet[i] {
                    return None;
                }
            }
            let last: Vec<char> = lines[s0 + n - 1].chars().collect();
            let last_needle: Vec<char> = snippet[n - 1].chars().collect();
            if last_needle.len() > last.len() || last[..last_needle.len()] != last_needle[..] {
                return None;
            }
            let ec = last_needle.len().saturating_sub(1);
            Some((sc, ec))
        }
    }
}

/// Index of the first occurrence of `needle` within `hay`.
fn find_subslice(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == needle[..])
}

// ---- Filename encoding -------------------------------------------------------

/// Percent-encode a repo-relative path into a single, filesystem-safe filename.
fn encode_filename(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len());
    for b in rel.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The current time as an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso8601_from_secs(secs)
}

/// Format Unix seconds as an ISO-8601 UTC timestamp.
fn iso8601_from_secs(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Convert days since the Unix epoch to a (year, month, day) civil date.
/// Howard Hinnant's algorithm (public domain), valid across the proleptic
/// Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// A best-effort unique id (creation time in nanoseconds).
fn unique_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn char_note_slices_single_line() {
        let ls = lines(&["let a = 1;", "let b = 2;"]);
        let note = Note::new(SelKind::Char, (1, 4), (1, 4), "x".into(), &ls).unwrap();
        assert_eq!(note.snippet, vec!["a"]);
    }

    #[test]
    fn char_note_slices_across_lines() {
        let ls = lines(&["foo(", "  bar,", ")"]);
        // from '(' on line 1 to 'bar' end on line 2
        let note = Note::new(SelKind::Char, (1, 3), (2, 4), "n".into(), &ls).unwrap();
        assert_eq!(note.snippet, vec!["(", "  bar"]);
    }

    #[test]
    fn line_note_keeps_whole_lines() {
        let ls = lines(&["a", "b", "c"]);
        let note = Note::new(SelKind::Line, (1, 0), (2, 0), "n".into(), &ls).unwrap();
        assert_eq!(note.snippet, vec!["a", "b"]);
    }

    #[test]
    fn revalidate_follows_a_shifted_line() {
        let ls = lines(&["one", "two", "three"]);
        let mut note = Note::new(SelKind::Line, (1, 0), (1, 0), "n".into(), &ls).unwrap();
        // Insert a line at the top; "one" is now line 2.
        let shifted = lines(&["zero", "one", "two", "three"]);
        assert!(revalidate(&mut note, &shifted));
        assert_eq!(note.start_line, 2);
    }

    #[test]
    fn revalidate_drops_deleted_text() {
        let ls = lines(&["keep", "delete-me"]);
        let mut note = Note::new(SelKind::Line, (2, 0), (2, 0), "n".into(), &ls).unwrap();
        let after = lines(&["keep"]);
        assert!(!revalidate(&mut note, &after));
    }

    #[test]
    fn char_note_relocates_within_a_line() {
        let ls = lines(&["value = compute()"]);
        let note = Note::new(SelKind::Char, (1, 8), (1, 14), "n".into(), &ls).unwrap();
        assert_eq!(note.snippet, vec!["compute"]);
        let mut note = note;
        let after = lines(&["let value =    compute()"]);
        assert!(revalidate(&mut note, &after));
        assert_eq!(note.start_col, 15);
        assert_eq!(note.end_col, 21);
    }

    #[test]
    fn filename_encoding_roundtrips_safely() {
        assert_eq!(encode_filename("src/app.rs"), "src%2Fapp.rs");
    }

    #[test]
    fn iso8601_formats_known_instants() {
        assert_eq!(iso8601_from_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_from_secs(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, to exercise the civil-date math.
        assert_eq!(iso8601_from_secs(1_582_934_400), "2020-02-29T00:00:00Z");
    }

    #[test]
    fn save_then_load_all_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let ls = lines(&["alpha", "beta"]);
        let note = Note::new(SelKind::Line, (1, 0), (1, 0), "hello".into(), &ls).unwrap();
        save(root, "src/app.rs", &[note]).unwrap();

        let loaded = load_all(root);
        let got = loaded.get("src/app.rs").expect("notes for path");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "hello");
        assert_eq!(got[0].snippet, vec!["alpha"]);

        // Saving an empty set removes the backing file.
        save(root, "src/app.rs", &[]).unwrap();
        assert!(load_all(root).is_empty());
    }

    #[test]
    fn contains_covers_char_and_line_ranges() {
        let ls = lines(&["abcdef"]);
        let note = Note::new(SelKind::Char, (1, 1), (1, 3), "n".into(), &ls).unwrap();
        assert!(note.contains(1, 2));
        assert!(!note.contains(1, 4));
        let line = Note::new(
            SelKind::Line,
            (2, 0),
            (3, 0),
            "n".into(),
            &lines(&["a", "b", "c"]),
        )
        .unwrap();
        assert!(line.contains(3, 99));
        assert!(!line.contains(1, 0));
    }
}
