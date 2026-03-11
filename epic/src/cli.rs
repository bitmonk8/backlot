use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "epic",
    about = "AI agent orchestration for software engineering tasks"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Credential name passed to Flick.
    #[arg(
        long,
        env = "EPIC_CREDENTIAL",
        default_value = "anthropic",
        global = true
    )]
    pub credential: String,

    /// Disable the TUI; run headless with event output to stderr.
    #[arg(long, env = "EPIC_NO_TUI", global = true)]
    pub no_tui: bool,

    /// Suppress the warning when no container/VM is detected.
    #[arg(long, env = "EPIC_NO_SANDBOX_WARN", global = true)]
    pub no_sandbox_warn: bool,
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
    /// One-time Windows setup: grant `AppContainer` access to the NUL device.
    Setup,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sandbox_warn_flag_parsed() {
        let cli = Cli::try_parse_from(["epic", "--no-sandbox-warn", "run", "test goal"])
            .expect("should parse");
        assert!(cli.no_sandbox_warn);
    }

    #[test]
    fn no_sandbox_warn_defaults_false() {
        let cli = Cli::try_parse_from(["epic", "run", "test goal"]).expect("should parse");
        assert!(!cli.no_sandbox_warn);
    }

    #[test]
    fn setup_command_parsed() {
        let cli = Cli::try_parse_from(["epic", "setup"]).expect("should parse");
        assert!(matches!(cli.command, Command::Setup));
    }
}
