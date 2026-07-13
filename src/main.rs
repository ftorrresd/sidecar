use anyhow::Result;
use clap::Parser;

mod app;
mod cli;
mod config;
mod diff;
mod ui;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Diff {
            old,
            new,
            watch,
            staged,
        } => {
            let files = if let (Some(old), Some(new)) = (&old, &new) {
                diff::diff_files(old, new)?
            } else {
                diff::git_diff(watch, staged)?
            };

            if watch {
                let watch_paths: Vec<_> = files
                    .iter()
                    .filter_map(|f| {
                        if f.path.contains('/') {
                            std::path::Path::new(&f.path)
                                .parent()
                                .map(|p| p.to_path_buf())
                        } else {
                            std::path::PathBuf::from(".").canonicalize().ok()
                        }
                    })
                    .collect();

                app::run_app(files, Some(watch_paths))?;
            } else {
                app::run_app(files, None)?;
            }
        }
        cli::Command::Show { revision } => {
            let files = diff::git_show(revision.as_deref())?;
            app::run_app(files, None)?;
        }
        cli::Command::Patch { file } => {
            let files = diff::parse_patch(file.as_deref())?;
            app::run_app(files, None)?;
        }
        cli::Command::Pager => {
            let files = diff::read_stdin_patch()?;
            app::run_app(files, None)?;
        }
    }

    Ok(())
}
