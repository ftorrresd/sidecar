//! Launching interactive external programs (yazi, fzf, ripgrep).
//!
//! Each of these takes over the terminal, so callers must suspend the ratatui
//! interface first (see `tui::suspend`). fzf reads keystrokes from /dev/tty, so
//! we can still feed it a list on stdin and read the selection from stdout.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::NamedTempFile;

/// A search hit: a file and, optionally, a 1-based line number.
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// Open yazi rooted at `root` in chooser mode; return the picked path (relative
/// to `root` when possible).
pub fn pick_with_yazi(root: &Path) -> Result<Option<PathBuf>> {
    let chooser = NamedTempFile::new().context("failed to create temp file")?;
    Command::new("yazi")
        .arg(root)
        .arg(format!("--chooser-file={}", chooser.path().display()))
        .status()
        .context("failed to run yazi")?;

    let selected = std::fs::read_to_string(chooser.path()).unwrap_or_default();
    Ok(first_path(&selected).map(|p| relativize(root, &p)))
}

/// `rg --files | fzf` — pick a filename from all project files.
pub fn pick_file_fzf(root: &Path) -> Result<Option<PathBuf>> {
    let mut rg = Command::new("rg")
        .args(["--files", "--hidden", "--glob", "!.git"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to run ripgrep")?;

    let rg_out = rg.stdout.take().context("ripgrep stdout unavailable")?;

    let out = Command::new("fzf")
        .current_dir(root)
        .args([
            "--prompt=file> ",
            "--header=Enter select · Esc cancel",
            "--preview=bat --color=always --style=numbers --line-range :300 {}",
            "--preview-window=right,60%,border-left",
        ])
        .stdin(rg_out)
        .stdout(Stdio::piped())
        .output()
        .context("failed to run fzf")?;

    let _ = rg.wait();
    let sel = String::from_utf8_lossy(&out.stdout);
    Ok(first_path(&sel).map(|p| relativize(root, &p)))
}

/// Interactive search over a prebuilt diff index (`path:line:col:text` records,
/// one per changed line). Filtering runs ripgrep over the index file, so the
/// results plug straight into the shared `path:line:col:text` parsing.
pub fn search_diff(root: &Path, index: &str) -> Result<Option<Hit>> {
    let mut tmp = NamedTempFile::new().context("failed to create temp file")?;
    tmp.write_all(index.as_bytes())
        .context("failed to write diff index")?;
    // `--no-line-number --no-filename` keeps ripgrep from adding its own prefix,
    // so each matching line is emitted verbatim as `path:line:col:text`.
    let cmd = format!(
        "rg --color=always --smart-case --no-line-number --no-filename -- {{q}} {} || true",
        shell_quote(tmp.path())
    );
    // `tmp` stays alive until the (synchronous) fzf call returns.
    reload_search(root, &cmd)
}

/// Open lazygit rooted at `root`.
pub fn open_lazygit(root: &Path) -> Result<()> {
    Command::new("lazygit")
        .arg("--path")
        .arg(root)
        .status()
        .context("failed to launch lazygit (is it installed?)")?;
    Ok(())
}

/// Open `file` in `$VISUAL`/`$EDITOR` (falling back to `vi`), at `line` when
/// known. Assumes a `+LINE`-style editor (vim, nvim, nano, emacs, helix…).
pub fn edit(file: &Path, line: Option<usize>) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let mut cmd = Command::new(program);
    for arg in parts {
        cmd.arg(arg);
    }
    if let Some(l) = line {
        cmd.arg(format!("+{l}"));
    }
    cmd.arg(file);
    cmd.status()
        .with_context(|| format!("failed to launch editor '{editor}'"))?;
    Ok(())
}

/// Shared fzf-in-reload-mode search used by `search_project`/`search_file`.
fn reload_search(root: &Path, reload_cmd: &str) -> Result<Option<Hit>> {
    let out = Command::new("fzf")
        .current_dir(root)
        .args([
            "--ansi",
            "--disabled",
            "--delimiter=:",
            "--with-nth=1,2,4..",
            "--prompt=search> ",
            "--header=Type a pattern · Enter jump · Esc cancel",
            &format!("--bind=start:reload:{reload_cmd}"),
            &format!("--bind=change:reload:sleep 0.05; {reload_cmd}"),
            "--preview=bat --color=always --style=numbers --highlight-line {2} {1}",
            "--preview-window=right,60%,border-left,+{2}/2",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .output()
        .context("failed to run fzf")?;

    let sel = String::from_utf8_lossy(&out.stdout);
    let Some(line) = sel.lines().next().filter(|l| !l.is_empty()) else {
        return Ok(None);
    };
    // Format: path:line:col:text
    let mut parts = line.splitn(4, ':');
    let path = parts.next().unwrap_or_default();
    let lineno = parts.next().and_then(|s| s.parse::<usize>().ok());
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(Hit {
        path: relativize(root, &PathBuf::from(path)),
        line: lineno,
    }))
}

/// First non-empty line of `text` as a path.
fn first_path(text: &str) -> Option<PathBuf> {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| PathBuf::from(l.trim()))
}

/// Make `path` relative to `root` when it lives inside the repo.
fn relativize(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(root).map(Path::to_path_buf).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// Minimal POSIX single-quote escaping for embedding a path in a shell command.
fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}
