//! CLI for the `lot` cross-platform process sandboxing library.
//!
//! Provides `lot run`, `lot setup`, and `lot probe` commands.

#![allow(clippy::print_stderr)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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

// ── Config deserialization ──────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct SandboxConfig {
    #[serde(default)]
    filesystem: FilesystemConfig,
    #[serde(default)]
    network: NetworkConfig,
    #[serde(default)]
    limits: LimitsConfig,
    #[serde(default)]
    environment: EnvironmentConfig,
    #[serde(default)]
    process: ProcessConfig,
}

#[derive(serde::Deserialize, Default)]
struct FilesystemConfig {
    #[serde(default)]
    read: Vec<PathBuf>,
    #[serde(default)]
    write: Vec<PathBuf>,
    #[serde(default)]
    exec: Vec<PathBuf>,
    #[serde(default)]
    deny: Vec<PathBuf>,
    #[serde(default)]
    include_platform_exec: bool,
    #[serde(default)]
    include_platform_lib: bool,
    #[serde(default)]
    include_temp: bool,
}

#[derive(serde::Deserialize, Default)]
struct NetworkConfig {
    #[serde(default)]
    allow: bool,
}

#[allow(clippy::struct_field_names)]
#[derive(serde::Deserialize, Default)]
struct LimitsConfig {
    max_memory_bytes: Option<u64>,
    max_processes: Option<u32>,
    max_cpu_seconds: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct EnvironmentConfig {
    #[serde(default)]
    forward_common: bool,
    #[serde(default)]
    vars: HashMap<String, String>,
}

#[derive(serde::Deserialize, Default)]
struct ProcessConfig {
    cwd: Option<PathBuf>,
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

    if config.environment.forward_common {
        cmd.forward_common_env();
    }
    for (key, val) in &config.environment.vars {
        cmd.env(key, val);
    }
    if let Some(ref cwd) = config.process.cwd {
        cmd.cwd(cwd);
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
    status.code().map_or(ExitCode::FAILURE, |code| {
        ExitCode::from(u8::try_from(code).unwrap_or(1))
    })
}

fn build_policy(config: &SandboxConfig) -> lot::Result<lot::SandboxPolicy> {
    let mut builder = lot::SandboxPolicyBuilder::new();

    for path in &config.filesystem.read {
        builder = builder.read_path(path);
    }
    for path in &config.filesystem.write {
        builder = builder.write_path(path);
    }
    for path in &config.filesystem.exec {
        builder = builder.exec_path(path);
    }
    for path in &config.filesystem.deny {
        builder = builder.deny_path(path);
    }
    if config.filesystem.include_platform_exec {
        builder = builder.include_platform_exec_paths();
    }
    if config.filesystem.include_platform_lib {
        builder = builder.include_platform_lib_paths();
    }
    if config.filesystem.include_temp {
        builder = builder.include_temp_dirs();
    }

    builder = builder.allow_network(config.network.allow);

    if let Some(bytes) = config.limits.max_memory_bytes {
        builder = builder.max_memory_bytes(bytes);
    }
    if let Some(n) = config.limits.max_processes {
        builder = builder.max_processes(n);
    }
    if let Some(secs) = config.limits.max_cpu_seconds {
        builder = builder.max_cpu_seconds(secs);
    }

    builder.build()
}

fn cmd_setup(args: &SetupArgs) -> ExitCode {
    #[cfg(target_os = "windows")]
    {
        let temp_dir = std::env::temp_dir();
        let policy = match lot::SandboxPolicyBuilder::new()
            .write_path(&temp_dir)
            .build()
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
    println!("cgroups_v2={}", caps.cgroups_v2);
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
