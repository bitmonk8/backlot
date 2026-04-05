//! CLI for the `lot` cross-platform process sandboxing library.
//!
//! Provides `lot run`, `lot setup`, and `lot probe` commands.

#![allow(clippy::print_stderr)]

mod config;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use config::{SandboxConfig, build_policy};

/// Cross-platform process sandboxing.
#[derive(Parser)]
#[command(name = "lot", about = "Cross-platform process sandboxing")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a program inside a sandbox.
    Run(RunArgs),
    /// Configure platform prerequisites.
    Setup(SetupArgs),
    /// Show platform sandboxing capabilities.
    Probe,
}

#[derive(clap::Args)]
struct RunArgs {
    /// Path to YAML sandbox config file.
    #[arg(short, long)]
    config: PathBuf,
    /// Wall-clock timeout in seconds.
    #[arg(short, long)]
    timeout: Option<u64>,
    /// Print sandbox setup details to stderr.
    #[arg(short, long)]
    verbose: bool,
    /// Validate config and print effective policy without spawning.
    #[arg(long)]
    dry_run: bool,
    /// Program and arguments (after --). Required unless --dry-run.
    #[arg(trailing_var_arg = true)]
    command: Vec<OsString>,
}

#[derive(clap::Args)]
struct SetupArgs {
    /// Check prerequisites without modifying anything.
    #[arg(long)]
    check: bool,
    /// Print details of what is being configured.
    #[arg(long)]
    verbose: bool,
}

// ── Command handlers ────────────────────────────────────────────────

fn cmd_run(args: &RunArgs) -> ExitCode {
    let config_text = match std::fs::read_to_string(&args.config) {
        Ok(text) => text,
        Err(e) => {
            eprintln!(
                "error: cannot read config file {}: {e}",
                args.config.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let config: SandboxConfig = match serde_yml::from_str(&config_text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: invalid config file {}: {e}", args.config.display());
            return ExitCode::FAILURE;
        }
    };

    let policy = match build_policy(&config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: policy build failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if args.dry_run {
        eprintln!("{policy:#?}");
        return ExitCode::SUCCESS;
    }

    if args.verbose {
        eprintln!("policy: {policy:#?}");
        let caps = lot::probe();
        eprintln!("platform capabilities: {caps:#?}");
    }

    let Some((program, cmd_args)) = args.command.split_first() else {
        eprintln!("error: no program specified after --");
        return ExitCode::FAILURE;
    };

    let mut cmd = lot::SandboxCommand::new(program);
    cmd.args(cmd_args);
    cmd.stdin(lot::SandboxStdio::Inherit);
    cmd.stdout(lot::SandboxStdio::Inherit);
    cmd.stderr(lot::SandboxStdio::Inherit);

    // User's explicit vars first so forward_common_env() skips duplicates.
    for (key, val) in &config.environment.vars {
        match crate::config::expand_vars(val) {
            Ok(expanded_val) => { cmd.env(key, expanded_val); }
            Err(e) => {
                eprintln!("error: env var expansion failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    if config.environment.forward_common {
        cmd.forward_common_env();
    }
    if let Some(ref cwd) = config.process.cwd {
        match crate::config::expand_path(cwd) {
            Ok(expanded_cwd) => { cmd.cwd(expanded_cwd); }
            Err(e) => {
                eprintln!("error: cwd expansion failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let child = match lot::spawn(&policy, &cmd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: sandbox spawn failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(timeout_secs) = args.timeout {
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("error: failed to create tokio runtime: {e}");
                return ExitCode::FAILURE;
            }
        };
        match rt.block_on(child.wait_with_output_timeout(timeout)) {
            Ok(output) => exit_code_from_status(output.status),
            Err(lot::SandboxError::Timeout(_)) => ExitCode::from(124),
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        match child.wait() {
            Ok(status) => exit_code_from_status(status),
            Err(e) => {
                eprintln!("error: wait failed: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

fn exit_code_from_status(status: std::process::ExitStatus) -> ExitCode {
    #[cfg(windows)]
    {
        // Windows exit codes are full 32-bit values; truncating to u8 loses
        // information (e.g. 0xC0000005 for access violation becomes 1).
        // We use process::exit() because ExitCode only supports u8 and there
        // is no way to propagate the full 32-bit exit code through main's
        // return value. This bypasses Drop impls, which is acceptable here
        // because cmd_run owns no resources that require cleanup at this point.
        status.code().map_or(ExitCode::FAILURE, |code| {
            std::process::exit(code);
        })
    }

    #[cfg(unix)]
    {
        // Unix exit codes are 8-bit (0-255).
        status.code().map_or(ExitCode::FAILURE, |code| {
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        })
    }
}

fn cmd_setup(args: &SetupArgs) -> ExitCode {
    #[cfg(target_os = "windows")]
    {
        let temp_dir = std::env::temp_dir();
        let policy = match lot::SandboxPolicyBuilder::new()
            .write_path(&temp_dir)
            .and_then(lot::SandboxPolicyBuilder::build)
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: failed to build setup policy: {e}");
                return ExitCode::FAILURE;
            }
        };

        if args.check {
            if args.verbose {
                eprintln!(
                    "Checking AppContainer prerequisites for {}",
                    temp_dir.display()
                );
            }
            if lot::appcontainer_prerequisites_met_for_policy(&policy) {
                eprintln!("Prerequisites met.");
                ExitCode::SUCCESS
            } else {
                eprintln!("Prerequisites NOT met. Run `lot setup` to configure.");
                ExitCode::FAILURE
            }
        } else {
            if args.verbose {
                eprintln!(
                    "Granting AppContainer prerequisites for {}",
                    temp_dir.display()
                );
            }
            match lot::grant_appcontainer_prerequisites_for_policy(&policy) {
                Ok(()) => {
                    if args.verbose {
                        eprintln!("Prerequisites granted.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: failed to grant prerequisites: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = args;
        eprintln!("No setup required on this platform.");
        ExitCode::SUCCESS
    }
}

fn cmd_probe() -> ExitCode {
    let caps = lot::probe();
    println!("appcontainer={}", caps.appcontainer);
    println!("job_objects={}", caps.job_objects);
    println!("namespaces={}", caps.namespaces);
    println!("seccomp={}", caps.seccomp);
    println!("seatbelt={}", caps.seatbelt);
    ExitCode::SUCCESS
}

// ── Entry point ─────────────────────────────────────────────────────

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(ref args) => cmd_run(args),
        Command::Setup(ref args) => cmd_setup(args),
        Command::Probe => cmd_probe(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    #[cfg(unix)]
    fn exit_code_from_status_success() {
        use std::process::Command;
        let status = Command::new("true").status().expect("run true");
        let code = exit_code_from_status(status);
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    #[cfg(unix)]
    fn exit_code_from_status_failure() {
        use std::process::Command;
        let status = Command::new("false").status().expect("run false");
        let code = exit_code_from_status(status);
        assert_eq!(code, ExitCode::from(1));
    }
    // Windows exit_code_from_status calls process::exit() which cannot be
    // tested in-process. The function is tested indirectly via integration.
}
