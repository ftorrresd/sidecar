use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "runk", about = "Review-first terminal diff viewer")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Review current repository changes
    Diff {
        /// First file (old)
        old: Option<String>,

        /// Second file (new)
        new: Option<String>,

        /// Watch for changes and auto-reload
        #[arg(long)]
        watch: bool,

        /// Show staged changes only
        #[arg(long)]
        staged: bool,
    },

    /// Review a specific commit
    Show {
        /// The revision to show (default: HEAD)
        revision: Option<String>,
    },

    /// Review a patch from a file or stdin
    Patch {
        /// Patch file path (reads from stdin if not provided or "-")
        file: Option<String>,
    },

    /// Act as a Git pager (reads diff from stdin)
    Pager,
}
