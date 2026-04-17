//! gate -- End-to-end test harness binary for the backlot workspace.
//!
//! Parses CLI args into a `GateConfig` and hands off to [`runner::run`],
//! which performs binary discovery, dispatches every requested stage,
//! prints the summary, and returns the process exit code.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

mod check;
mod exec;
mod prereqs;
mod report;
mod runner;
mod scratch;
mod stage;
mod types;

use types::{GateConfig, Stage};

/// CLI for the `gate` end-to-end harness.
#[derive(Debug, Parser)]
#[command(
    name = "gate",
    version,
    about = "End-to-end test harness for the backlot workspace"
)]
struct Cli {
    /// Run exactly one stage.
    #[arg(long, value_name = "STAGE", conflicts_with = "from")]
    only: Option<Stage>,

    /// Resume from a stage; earlier stages are skipped.
    #[arg(long, value_name = "STAGE")]
    from: Option<Stage>,

    /// Write results.json to the output directory; implies --keep-scratch.
    /// Transcripts are deferred to a later deliverable.
    #[arg(long)]
    verbose: bool,

    /// Override the directory used to discover backlot binaries.
    #[arg(long, value_name = "PATH")]
    bin_dir: Option<PathBuf>,

    /// Per-stage wall-clock timeout, in seconds.
    #[arg(long, value_name = "SECONDS")]
    timeout: Option<u64>,

    /// Where to write results.json (transcripts deferred to a later deliverable).
    #[arg(long, value_name = "PATH", default_value = "gate/output/")]
    output_dir: PathBuf,

    /// Preserve the per-run scratch directory even on success.
    #[arg(long)]
    keep_scratch: bool,
}

impl Cli {
    fn into_config(self) -> GateConfig {
        GateConfig {
            only: self.only,
            from: self.from,
            verbose: self.verbose,
            bin_dir: self.bin_dir,
            timeout: self.timeout.map(Duration::from_secs),
            output_dir: self.output_dir,
            keep_scratch: self.keep_scratch,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = cli.into_config();
    // Process exit codes only need 0/1/2; bridging an i32 sentinel into
    // `ExitCode` requires a u8.
    let code = runner::run(config);
    let exit = if (0..=255).contains(&code) {
        code as u8
    } else {
        // Out-of-range sentinel from runner -- treat as prereq failure rather
        // than silently mapping a negative value to success (0).
        2
    };
    ExitCode::from(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_defaults() {
        let cli = Cli::try_parse_from(["gate"]).expect("default parse");
        let cfg = cli.into_config();
        assert_eq!(cfg.output_dir, PathBuf::from("gate/output/"));
        assert!(!cfg.verbose);
        assert!(!cfg.keep_scratch);
        assert!(cfg.only.is_none());
        assert!(cfg.from.is_none());
        assert!(cfg.bin_dir.is_none());
        assert!(cfg.timeout.is_none());
    }

    #[test]
    fn cli_only_and_from_conflict() {
        let err = Cli::try_parse_from(["gate", "--only", "flick", "--from", "lot"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn cli_only_valid_stage() {
        let cli = Cli::try_parse_from(["gate", "--only", "flick"]).expect("only flick");
        assert_eq!(cli.only, Some(Stage::Flick));
        assert!(cli.from.is_none());
    }

    #[test]
    fn cli_invalid_stage() {
        let err = Cli::try_parse_from(["gate", "--only", "bogus"]).unwrap_err();
        // Clap reports value-parser failures as ValueValidation.
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn cli_timeout_parsed() {
        let cli = Cli::try_parse_from(["gate", "--timeout", "120"]).expect("timeout 120");
        let cfg = cli.into_config();
        assert_eq!(cfg.timeout, Some(Duration::from_secs(120)));
    }

    #[test]
    fn cli_help_compiles() {
        // Smoke-test that the clap command is well-formed; debug_assert
        // catches duplicate names, bad combinations, etc.
        Cli::command().debug_assert();
    }
}
