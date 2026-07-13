//! sidecar — a side-by-side diff/review TUI for agentic coding workflows.
//!
//! Open it next to your coding agent. It shows the project-wide diff by default;
//! select a file to see its diff (or its contents when unchanged). Keys let you
//! jump around with yazi, fzf, and ripgrep.

mod app;
mod external;
mod git;
mod render;
mod tui;

use anyhow::{bail, Context, Result};
use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn main() -> Result<()> {
    if let Err(e) = real_main() {
        // Make sure the terminal is usable before printing the error.
        let _ = tui::restore();
        eprintln!("sidecar: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

fn real_main() -> Result<()> {
    ensure_tools()?;

    // Optional positional arg: a directory to start in.
    let start = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);

    let root = git::top_level(&start)
        .with_context(|| format!("{} is not inside a git repository", start.display()))?;

    let mut terminal = tui::init()?;
    let result = app::App::new(root).and_then(|mut a| a.run(&mut terminal));
    tui::restore()?;
    result
}

/// sidecar orchestrates external programs rather than reimplementing them, so
/// they must be installed. The core renderers (git/delta/bat) are required up
/// front; the on-demand tools (yazi/fzf/ripgrep/lazygit) fail with a message
/// only if/when their key is pressed.
fn ensure_tools() -> Result<()> {
    const REQUIRED: [&str; 3] = ["git", "delta", "bat"];
    let missing: Vec<&str> = REQUIRED.iter().copied().filter(|t| !tool_exists(t)).collect();
    if !missing.is_empty() {
        bail!(
            "missing required tools: {}. On Arch: sudo pacman -S --needed git git-delta bat \
             yazi ripgrep fzf lazygit",
            missing.join(", ")
        );
    }
    Ok(())
}

/// Whether `tool` can be spawned (i.e. is on `PATH`).
fn tool_exists(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}
