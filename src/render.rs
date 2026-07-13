//! Turn raw diffs / file contents into colored, terminal-ready text.
//!
//! `delta` formats diffs and `bat` formats plain files; both emit ANSI escape
//! sequences which `ansi-to-tui` converts into ratatui `Text` for display.

use anyhow::{Context, Result};
use ansi_to_tui::IntoText;
use ratatui::text::Text;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Pipe a raw unified diff through `delta` and return colored ANSI bytes.
///
/// The diff is written on a dedicated thread while stdout is drained on this
/// one; otherwise a large diff deadlocks (delta blocks writing stdout once the
/// pipe buffer fills, while we are still blocked writing its stdin).
fn delta(raw_diff: &str, width: u16, side_by_side: bool, wrap: bool) -> Result<Vec<u8>> {
    let mut cmd = Command::new("delta");
    cmd.args([
        "--paging=never",
        "--line-numbers",
        "--width",
        &width.to_string(),
        // Control wrapping explicitly so it doesn't depend on delta's defaults
        // (side-by-side wraps by default): unlimited = wrap, 0 = truncate.
        "--wrap-max-lines",
        if wrap { "unlimited" } else { "0" },
    ]);
    if side_by_side {
        cmd.arg("--side-by-side");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn delta")?;

    let mut stdin = child.stdin.take().context("delta stdin unavailable")?;
    let raw = raw_diff.to_owned();
    let writer = std::thread::spawn(move || {
        // Ignore write errors (e.g. delta exits early on malformed input);
        // dropping `stdin` at the end signals EOF.
        let _ = stdin.write_all(raw.as_bytes());
    });

    let output = child.wait_with_output().context("delta failed")?;
    let _ = writer.join();
    Ok(output.stdout)
}

/// Render a file's contents through `bat` and return colored ANSI bytes.
fn bat(path: &Path, width: u16, wrap: bool) -> Result<Vec<u8>> {
    let output = Command::new("bat")
        .args([
            "--color=always",
            "--style=numbers,changes",
            "--paging=never",
            "--wrap",
            if wrap { "auto" } else { "never" },
            "--terminal-width",
            &width.to_string(),
        ])
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .context("failed to run bat")?;
    Ok(output.stdout)
}

/// Convert ANSI bytes into ratatui `Text`, falling back to lossy plain text.
fn ansi_to_text(bytes: Vec<u8>) -> Text<'static> {
    match bytes.as_slice().into_text() {
        Ok(text) => text,
        Err(_) => Text::raw(String::from_utf8_lossy(&bytes).into_owned()),
    }
}

/// Colored diff text for display.
pub fn diff_text(raw_diff: &str, width: u16, side_by_side: bool, wrap: bool) -> Text<'static> {
    if raw_diff.trim().is_empty() {
        return Text::raw("No changes.");
    }
    match delta(raw_diff, width, side_by_side, wrap) {
        Ok(bytes) => ansi_to_text(bytes),
        Err(e) => Text::raw(format!("delta error: {e}")),
    }
}

/// Colored file-content text for display.
pub fn content_text(path: &Path, width: u16, wrap: bool) -> Text<'static> {
    if !path.exists() {
        return Text::raw(format!("File not found: {}", path.display()));
    }
    match bat(path, width, wrap) {
        Ok(bytes) => ansi_to_text(bytes),
        Err(e) => Text::raw(format!("bat error: {e}")),
    }
}
