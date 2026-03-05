use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "epic", about = "AI agent orchestration for software engineering tasks")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Path to the Flick executable.
    #[arg(long, env = "EPIC_FLICK_PATH", default_value = "flick", global = true)]
    pub flick_path: PathBuf,

    /// Credential name passed to Flick.
    #[arg(long, env = "EPIC_CREDENTIAL", default_value = "anthropic", global = true)]
    pub credential: String,

    /// Disable the TUI; run headless with event output to stderr.
    #[arg(long, env = "EPIC_NO_TUI", global = true)]
    pub no_tui: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize epic.toml via agent-driven project exploration.
    Init,
    /// Start a new run with the given goal.
    Run {
        /// The goal to accomplish.
        goal: String,
    },
    /// Resume a previously interrupted run from .epic/state.json.
    Resume,
    /// Show the current status of a run.
    Status,
}
