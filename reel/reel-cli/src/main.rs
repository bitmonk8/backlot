// reel CLI: thin binary over the reel library.
//
// Handles argument parsing, config loading, stdin/stdout formatting,
// and tokio runtime bootstrap. All agent logic lives in the reel crate.

use clap::{Parser, Subcommand};
use serde::Serialize;
use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

// ---------------------------------------------------------------------------
// CLI structure
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "reel", about = "Agent session runner")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an agent session.
    Run(RunArgs),
    /// Configure platform prerequisites.
    Setup(SetupArgs),
}

#[derive(clap::Args)]
struct RunArgs {
    /// Path to reel config file (YAML).
    #[arg(long)]
    config: PathBuf,

    /// Query text. If omitted, reads from stdin.
    #[arg(long)]
    query: Option<String>,

    /// Working directory for tool execution (default: cwd).
    #[arg(long)]
    project_root: Option<PathBuf>,

    /// Per-tool-call timeout in seconds (default: 120).
    #[arg(long, default_value_t = 120)]
    timeout: u64,

    /// Build the request and print it without calling the model.
    #[arg(long)]
    dry_run: bool,
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

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SuccessOutput {
    status: &'static str,
    content: serde_json::Value,
    usage: Option<UsageOutput>,
    tool_calls: u32,
    response_hash: Option<String>,
}

#[derive(Serialize)]
struct UsageOutput {
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
}

#[derive(Serialize)]
struct ErrorOutput {
    status: &'static str,
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

// ---------------------------------------------------------------------------
// Config parsing
// ---------------------------------------------------------------------------

/// Parse once as generic YAML, pop reel-specific `grant` key, pass remainder to flick.
fn parse_config(text: &str) -> Result<(reel::RequestConfig, reel::ToolGrant), String> {
    let mut map: serde_yml::Value =
        serde_yml::from_str(text).map_err(|e| format!("config parse: {e}"))?;

    let grant = match map.get("grant") {
        Some(serde_yml::Value::Sequence(names)) => {
            let strs: Vec<String> = names
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| "grant entries must be strings".to_string())
                })
                .collect::<Result<_, _>>()?;
            reel::ToolGrant::from_names(&strs).map_err(|e| e.to_string())?
        }
        Some(serde_yml::Value::Null) | None => reel::ToolGrant::empty(),
        Some(_) => return Err("grant must be a list of strings".into()),
    };

    if let serde_yml::Value::Mapping(ref mut m) = map {
        m.remove(serde_yml::Value::String("grant".into()));
    }

    let stripped = serde_yml::to_string(&map).map_err(|e| format!("config re-serialize: {e}"))?;
    let config = reel::RequestConfig::from_str(&stripped, reel::ConfigFormat::Yaml)
        .map_err(|e| format!("request config: {e}"))?;

    Ok((config, grant))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn cmd_run(args: RunArgs) -> Result<(), String> {
    // Load and parse config.
    let config_text = tokio::fs::read_to_string(&args.config)
        .await
        .map_err(|e| format!("failed to read config {}: {e}", args.config.display()))?;

    let (request_config, grant) = parse_config(&config_text)?;

    let request = reel::AgentRequestConfig {
        config: request_config,
        grant,
        custom_tools: Vec::new(),
    };

    // Dry run: build the effective config and print it without calling the model.
    if args.dry_run {
        let effective =
            reel::Agent::build_effective_config(&request).map_err(|e| format!("{e}"))?;
        let dry_output = serde_json::json!({
            "model": effective.model(),
            "system_prompt": effective.system_prompt(),
            "temperature": effective.temperature(),
            "tools": effective.tools().iter().map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters()
                })
            }).collect::<Vec<_>>(),
        });
        let json =
            serde_json::to_string_pretty(&dry_output).map_err(|e| format!("serialize: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    // Resolve query: --query flag wins, otherwise stdin.
    let query = match args.query {
        Some(q) => q,
        None => {
            if std::io::stdin().is_terminal() {
                eprintln!("Reading query from stdin (Ctrl+D to submit)...");
            }
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("failed to read stdin: {e}"))?;
            buf
        }
    };

    if query.trim().is_empty() {
        return Err("query is empty".into());
    }

    // Build environment.
    let project_root = match args.project_root {
        Some(p) => p,
        None => std::env::current_dir()
            .map_err(|e| format!("cannot determine current directory: {e}"))?,
    };

    let model_registry = reel::ModelRegistry::load_default()
        .await
        .map_err(|e| format!("failed to load model registry: {e}"))?;
    let provider_registry = reel::ProviderRegistry::load_default()
        .map_err(|e| format!("failed to load provider registry: {e}"))?;

    let env = reel::AgentEnvironment {
        model_registry,
        provider_registry,
        project_root,
        timeout: Duration::from_secs(args.timeout),
    };

    let agent = reel::Agent::new(env);

    // Always deserialize as Value. When output_schema is set the model
    // returns structured JSON that parses directly. Without a schema the
    // model returns free-form text which serde_json::from_str would reject,
    // so reel's finalize_result wraps it in Value::String for us.
    let result: reel::RunResult<serde_json::Value> = agent
        .run(&request, &query)
        .await
        .map_err(|e| format!("{e}"))?;

    let output = SuccessOutput {
        status: "Ok",
        content: result.output,
        usage: result.usage.map(|u| UsageOutput {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cost_usd: u.cost_usd,
        }),
        tool_calls: result.tool_calls,
        response_hash: result.response_hash,
    };
    let json = serde_json::to_string(&output).map_err(|e| format!("serialize: {e}"))?;
    println!("{json}");

    Ok(())
}

fn cmd_setup(args: &SetupArgs) -> Result<(), String> {
    if args.check {
        eprintln!("Checking prerequisites...");
        check_windows_prerequisites(args.verbose)
    } else {
        eprintln!("Configuring prerequisites...");
        configure_windows_prerequisites(args.verbose)
    }
}

#[cfg(target_os = "windows")]
fn check_windows_prerequisites(verbose: bool) -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?;
    let paths: Vec<&std::path::Path> = vec![cwd.as_path()];
    let ok = reel::sandbox::appcontainer_prerequisites_met(&paths);

    if verbose {
        eprintln!(
            "AppContainer prerequisites: {}",
            if ok { "OK" } else { "MISSING" }
        );
    }

    if !ok {
        return Err("AppContainer prerequisites not configured. Run `reel setup` to fix.".into());
    }

    eprintln!("All prerequisites OK.");
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_windows_prerequisites(verbose: bool) -> Result<(), String> {
    if verbose {
        eprintln!("Granting AppContainer prerequisites (NUL device + ancestor traverse ACEs)...");
    }

    let cwd = std::env::current_dir().map_err(|e| format!("failed to get cwd: {e}"))?;
    let paths: Vec<&std::path::Path> = vec![cwd.as_path()];

    reel::sandbox::grant_appcontainer_prerequisites(&paths).map_err(|e| {
        format!("Failed to grant AppContainer prerequisites: {e}. Try running as administrator.")
    })?;

    eprintln!("Setup complete.");
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::unnecessary_wraps)]
fn check_windows_prerequisites(_verbose: bool) -> Result<(), String> {
    eprintln!("No setup required on this platform.");
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[allow(clippy::unnecessary_wraps)]
fn configure_windows_prerequisites(_verbose: bool) -> Result<(), String> {
    eprintln!("No setup required on this platform.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

fn emit_error(code: &str, message: &str) {
    let output = ErrorOutput {
        status: "Error",
        error: ErrorDetail {
            code: code.into(),
            message: message.into(),
        },
    };
    if let Ok(json) = serde_json::to_string(&output) {
        println!("{json}");
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Run(args) => cmd_run(args).await,
        Commands::Setup(args) => cmd_setup(&args),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            emit_error("cli_error", &msg);
            ExitCode::FAILURE
        }
    }
}
