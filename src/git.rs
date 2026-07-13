//! Thin wrappers around the installed `git` executable.
//!
//! We shell out to the user's `git` (rather than linking libgit2) so that the
//! tool honors their exact git version, config, attributes, filters and diff
//! drivers — the same output they would see on the command line.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A single entry from `git status --porcelain`.
#[derive(Clone, Debug)]
pub struct ChangedFile {
    /// Path relative to the repository root.
    pub path: String,
    /// Two-character status code, e.g. " M", "??", "A ", "MM".
    pub status: String,
}

impl ChangedFile {
    /// A compact one-character marker for the file list.
    pub fn marker(&self) -> char {
        let s = self.status.as_bytes();
        match (s.first().copied(), s.get(1).copied()) {
            (Some(b'?'), _) => '?',                   // untracked
            (Some(b'A'), _) => 'A',                   // added
            (Some(b'D'), _) | (_, Some(b'D')) => 'D', // deleted
            (Some(b'R'), _) => 'R',                   // renamed
            (Some(b' '), Some(b'M')) => 'M',          // modified, unstaged
            (Some(b'M'), _) => 'M',                   // modified, staged
            _ => '~',
        }
    }
}

/// Run a command and capture stdout, erroring on a non-zero exit.
fn capture(cmd: &mut Command) -> Result<String> {
    let output = cmd.output().context("failed to spawn command")?;
    if !output.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(root);
    c
}

/// A `git` command preconfigured for producing diffs we parse: literal paths and
/// standard `a/`,`b/` prefixes regardless of the user's `diff.mnemonicPrefix` /
/// `diff.noprefix` settings.
fn git_diff(root: &Path) -> Command {
    let mut c = git(root);
    c.args([
        "-c",
        "core.quotepath=false",
        "-c",
        "diff.mnemonicPrefix=false",
        "-c",
        "diff.noprefix=false",
    ]);
    c
}

/// Resolve the repository root for `start`, or error if not in a repo.
pub fn top_level(start: &Path) -> Result<PathBuf> {
    let out = capture(
        Command::new("git")
            .current_dir(start)
            .args(["rev-parse", "--show-toplevel"]),
    )
    .context("not inside a git repository")?;
    Ok(PathBuf::from(out.trim()))
}

/// Whether the repository has at least one commit (a resolvable HEAD).
pub fn has_head(root: &Path) -> bool {
    git(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// List all changed files (tracked modifications + untracked), relative to root.
pub fn changed_files(root: &Path) -> Result<Vec<ChangedFile>> {
    let out = capture(git(root).args([
        "-c",
        "core.quotepath=false",
        "status",
        "--porcelain=v1",
        // `all` (not `normal`) so untracked *files* are listed individually
        // rather than collapsed into their parent directory (e.g. `.github/`).
        "--untracked-files=all",
        "--no-renames",
    ]))?;

    let mut files = Vec::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[..2].to_string();
        // Porcelain format: "XY <path>" (renames use " -> " which --no-renames avoids).
        let path = line[3..].to_string();
        files.push(ChangedFile { path, status });
    }
    Ok(files)
}

/// List all tracked and untracked files, excluding ignored files.
pub fn all_files(root: &Path) -> Result<Vec<String>> {
    let out = capture(git(root).args([
        "-c",
        "core.quotepath=false",
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
    ]))?;
    let mut files: Vec<String> = out
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// Whether `path` is tracked by git.
pub fn is_tracked(root: &Path, path: &str) -> bool {
    git(root)
        .args(["ls-files", "--error-unmatch", "--", path])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether `path` (a tracked file) differs from HEAD (or the index if no HEAD).
pub fn file_has_diff(root: &Path, path: &str) -> Result<bool> {
    let mut cmd = git(root);
    if has_head(root) {
        cmd.args(["diff", "--quiet", "HEAD", "--", path]);
    } else {
        cmd.args(["diff", "--cached", "--quiet", "--", path]);
    }
    let status = cmd.status().context("git diff --quiet failed to run")?;
    match status.code() {
        Some(0) => Ok(false), // no difference
        Some(1) => Ok(true),  // differs
        other => bail!("git diff --quiet exited with {:?}", other),
    }
}

/// Raw (uncolored) unified diff for the whole project, ready to feed to delta.
///
/// Covers tracked staged+unstaged changes against HEAD, plus untracked files
/// rendered as additions from /dev/null.
pub fn project_diff_raw(root: &Path) -> Result<String> {
    let mut out = String::new();

    if has_head(root) {
        out.push_str(&capture(git_diff(root).args([
            "diff",
            "--no-color",
            "HEAD",
            "--",
        ]))?);
    } else {
        out.push_str(&capture(git_diff(root).args([
            "diff",
            "--no-color",
            "--cached",
            "--",
        ]))?);
    }

    for path in untracked_files(root)? {
        out.push_str(&no_index_diff(root, &path));
    }

    Ok(out)
}

/// Raw unified diff for a single tracked, changed file.
pub fn file_diff_raw(root: &Path, path: &str) -> Result<String> {
    if has_head(root) {
        capture(git_diff(root).args(["diff", "--no-color", "HEAD", "--", path]))
    } else {
        capture(git_diff(root).args(["diff", "--no-color", "--cached", "--", path]))
    }
}

/// List untracked (but not ignored) files relative to root.
pub fn untracked_files(root: &Path) -> Result<Vec<String>> {
    let out = capture(git(root).args([
        "-c",
        "core.quotepath=false",
        "ls-files",
        "--others",
        "--exclude-standard",
    ]))?;
    Ok(out.lines().map(str::to_owned).collect())
}

/// Raw `git diff --no-index` for a single untracked file (public wrapper).
pub fn untracked_diff_raw(root: &Path, path: &str) -> String {
    no_index_diff(root, path)
}

/// Build a searchable index of the *changed* lines of a raw unified diff.
///
/// Each added or removed line becomes one `path:line:col:text` record — the
/// same shape fzf/ripgrep produce — so diff search plugs into the existing
/// jump-to-file flow. `line` is the new-file line number (removed lines point
/// at where they were deleted). Context lines are omitted so the search only
/// covers what actually changed.
pub fn index_diff(raw: &str) -> String {
    let mut out = String::new();
    let mut path = String::new();
    let mut new_line: usize = 0;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            path = normalize_diff_path(rest);
        } else if line.starts_with("--- ") || line.starts_with("diff ") {
            // Header lines; nothing to index.
        } else if let Some(rest) = line.strip_prefix("@@") {
            // @@ -a,b +c,d @@ — grab the new-file start line `c`.
            new_line = parse_hunk_new_start(rest);
        } else if let Some(text) = line.strip_prefix('+') {
            if !path.is_empty() {
                out.push_str(&format!("{path}:{new_line}:1:+{text}\n"));
            }
            new_line += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            if !path.is_empty() {
                out.push_str(&format!("{path}:{new_line}:1:-{text}\n"));
            }
            // Removed line: the new-file cursor does not advance.
        } else if line.starts_with(' ') {
            new_line += 1;
        }
    }
    out
}

/// Turn a diff path token into a clean, repo-relative path.
///
/// Strips the diff prefix — standard `a/`,`b/` or git's mnemonic `c/`,`i/`,`w/`,
/// `o/` (used when `diff.mnemonicPrefix` is enabled) — and drops `/dev/null`.
fn normalize_diff_path(token: &str) -> String {
    let t = token.trim();
    if t == "/dev/null" {
        return String::new();
    }
    for prefix in ["a/", "b/", "c/", "i/", "w/", "o/"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    t.to_string()
}

/// Parse the new-file start line from a hunk header body like ` -1,4 +2,6 @@ ...`.
fn parse_hunk_new_start(rest: &str) -> usize {
    rest.split('+')
        .nth(1)
        .and_then(|s| s.split([',', ' ']).next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1)
}

/// Render an untracked file as a `git diff --no-index` addition.
///
/// This intentionally ignores the exit status: `--no-index` returns 1 whenever
/// the inputs differ (always, since we compare against /dev/null).
fn no_index_diff(root: &Path, path: &str) -> String {
    let output = git_diff(root)
        .args(["diff", "--no-color", "--no-index", "--", "/dev/null", path])
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn index_diff_records_added_and_removed_with_new_line_numbers() {
        let raw = "\
diff --git a/src/x.rs b/src/x.rs
--- a/src/x.rs
+++ b/src/x.rs
@@ -1,3 +1,4 @@
 fn main() {
-    let a = 1;
+    let a = 2;
+    let b = 3;
 }
";
        let index = index_diff(raw);
        let lines: Vec<&str> = index.lines().collect();
        assert_eq!(
            lines,
            vec![
                // removed line keeps the new-file cursor (line 2)
                "src/x.rs:2:1:-    let a = 1;",
                // added lines advance from line 2
                "src/x.rs:2:1:+    let a = 2;",
                "src/x.rs:3:1:+    let b = 3;",
            ]
        );
    }

    #[test]
    fn parse_hunk_new_start_reads_the_plus_side() {
        assert_eq!(parse_hunk_new_start(" -1,3 +5,6 @@ fn foo()"), 5);
        assert_eq!(parse_hunk_new_start(" -0,0 +1 @@"), 1);
    }

    #[test]
    fn all_files_includes_tracked_and_untracked_but_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(git(root).args(["init", "-q"]).status().unwrap().success());

        fs::write(root.join("tracked.txt"), "tracked").unwrap();
        fs::write(root.join("untracked.txt"), "untracked").unwrap();
        fs::write(root.join("ignored.txt"), "ignored").unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        assert!(git(root)
            .args(["add", "tracked.txt"])
            .status()
            .unwrap()
            .success());
        fs::remove_file(root.join("tracked.txt")).unwrap();

        assert_eq!(
            all_files(root).unwrap(),
            vec![".gitignore", "tracked.txt", "untracked.txt"]
        );
    }
}
