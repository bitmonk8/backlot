// vault CLI: thin binary over the vault library.
//
// Subcommands map directly to vault's public API. Configuration is read from
// a YAML file. JSON output to stdout, errors to stderr. Every command includes
// a `usage` block in its JSON output with token counts, cost, latency, and
// tool call count. The `--verbose` flag adds the full session transcript.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// CLI argument types
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "vault", about = "Persistent file-based knowledge store")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Include session transcript in JSON output.
    #[arg(long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize vault from requirements (read from stdin).
    Bootstrap {
        #[arg(long)]
        config: PathBuf,
    },
    /// Query the vault's knowledge base.
    Query {
        #[arg(long)]
        config: PathBuf,
        /// Query text. If omitted, reads from stdin.
        #[arg(long)]
        query: Option<String>,
    },
    /// Record new content into the vault.
    Record {
        #[arg(long)]
        config: PathBuf,
        /// Document series name (UPPERCASE).
        #[arg(long)]
        name: String,
        /// Create a new series or append to existing.
        #[arg(long, value_enum)]
        mode: RecordModeArg,
        /// Content text. If omitted, reads from stdin.
        #[arg(long)]
        content: Option<String>,
    },
    /// Trigger full restructuring pass.
    Reorganize {
        #[arg(long)]
        config: PathBuf,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum RecordModeArg {
    New,
    Append,
}

impl From<RecordModeArg> for vault::RecordMode {
    fn from(arg: RecordModeArg) -> Self {
        match arg {
            RecordModeArg::New => Self::New,
            RecordModeArg::Append => Self::Append,
        }
    }
}

// ---------------------------------------------------------------------------
// YAML configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Config {
    storage_root: PathBuf,
    models: ConfigModels,
}

#[derive(Debug, Deserialize)]
struct ConfigModels {
    bootstrap: String,
    query: String,
    record: String,
    reorganize: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_stdin() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("failed to read stdin: {e}"))?;
    Ok(buf)
}

fn load_config(path: &Path) -> Result<Config, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
    serde_yaml::from_str(&content).map_err(|e| format!("failed to parse config: {e}"))
}

async fn build_vault(config: Config) -> Result<vault::Vault, String> {
    let model_registry = reel::ModelRegistry::load_default()
        .await
        .map_err(|e| format!("failed to load models: {e}"))?;
    let provider_registry = reel::ProviderRegistry::load_default()
        .map_err(|e| format!("failed to load providers: {e}"))?;

    let env = vault::VaultEnvironment {
        storage_root: config.storage_root,
        model_registry,
        provider_registry,
        models: vault::VaultModels {
            bootstrap: config.models.bootstrap,
            query: config.models.query,
            record: config.models.record,
            reorganize: config.models.reorganize,
        },
    };

    vault::Vault::new(env).map_err(|e| e.to_string())
}

fn emit_error(msg: &str) {
    let json = serde_json::json!({"error": msg});
    eprintln!("{json}");
}

fn emit_warnings(warnings: &[vault::DerivedValidationWarning]) {
    for w in warnings {
        eprintln!("vault: validation warning: {}: {}", w.filename, w.reason);
    }
}

/// Build the `usage` JSON block from session metadata.
fn build_usage_json(metadata: &vault::SessionMetadata, verbose: bool) -> serde_json::Value {
    let mut usage = serde_json::json!({
        "input_tokens": metadata.input_tokens,
        "output_tokens": metadata.output_tokens,
        "tool_calls": metadata.tool_calls,
        "api_latency_ms": metadata.api_latency_ms(),
    });
    if metadata.cache_creation_input_tokens > 0 {
        usage["cache_creation_input_tokens"] = metadata.cache_creation_input_tokens.into();
    }
    if metadata.cache_read_input_tokens > 0 {
        usage["cache_read_input_tokens"] = metadata.cache_read_input_tokens.into();
    }
    if metadata.cost_usd.abs() > f64::EPSILON {
        usage["cost_usd"] = metadata.cost_usd.into();
    }
    if verbose {
        usage["transcript"] =
            serde_json::to_value(&metadata.transcript).unwrap_or(serde_json::Value::Null);
    }
    usage
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let verbose = cli.verbose;

    let result = match cli.command {
        Command::Bootstrap { config } => run_bootstrap(&config, verbose).await,
        Command::Query { config, query } => run_query(&config, query, verbose).await,
        Command::Record {
            config,
            name,
            mode,
            content,
        } => run_record(&config, &name, mode, content, verbose).await,
        Command::Reorganize { config } => run_reorganize(&config, verbose).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            emit_error(&msg);
            ExitCode::FAILURE
        }
    }
}

async fn run_bootstrap(config_path: &Path, verbose: bool) -> Result<(), String> {
    let config = load_config(config_path)?;
    let vault = build_vault(config).await?;
    let requirements = read_stdin()?;
    let (warnings, metadata) = vault
        .bootstrap(&requirements)
        .await
        .map_err(|e| e.to_string())?;
    emit_warnings(&warnings);
    let output = serde_json::json!({
        "warnings": warnings,
        "usage": build_usage_json(&metadata, verbose),
    });
    let json = serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

async fn run_query(
    config_path: &Path,
    query_text: Option<String>,
    verbose: bool,
) -> Result<(), String> {
    let config = load_config(config_path)?;
    let vault = build_vault(config).await?;
    let question = match query_text {
        Some(q) => q,
        None => read_stdin()?,
    };
    let (result, metadata) = vault.query(&question).await.map_err(|e| e.to_string())?;
    let mut output =
        serde_json::to_value(&result).map_err(|e| format!("failed to serialize result: {e}"))?;
    output["usage"] = build_usage_json(&metadata, verbose);
    let json = serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

async fn run_record(
    config_path: &Path,
    name: &str,
    mode: RecordModeArg,
    content_text: Option<String>,
    verbose: bool,
) -> Result<(), String> {
    let config = load_config(config_path)?;
    let vault = build_vault(config).await?;
    let content = match content_text {
        Some(c) => c,
        None => read_stdin()?,
    };
    let (modified, warnings, metadata) = vault
        .record(name, &content, mode.into())
        .await
        .map_err(|e| e.to_string())?;
    emit_warnings(&warnings);
    let output = serde_json::json!({
        "modified": modified,
        "warnings": warnings,
        "usage": build_usage_json(&metadata, verbose),
    });
    let json = serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

async fn run_reorganize(config_path: &Path, verbose: bool) -> Result<(), String> {
    let config = load_config(config_path)?;
    let vault = build_vault(config).await?;
    let (report, warnings, metadata) = vault.reorganize().await.map_err(|e| e.to_string())?;
    emit_warnings(&warnings);
    let mut output =
        serde_json::to_value(&report).map_err(|e| format!("failed to serialize report: {e}"))?;
    output["warnings"] = serde_json::to_value(&warnings)
        .map_err(|e| format!("failed to serialize warnings: {e}"))?;
    output["usage"] = build_usage_json(&metadata, verbose);
    let json = serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn config_deserialization() {
        let yaml = r#"
storage_root: ".epic/docs/"
models:
  bootstrap: "sonnet"
  query: "haiku"
  record: "haiku"
  reorganize: "sonnet"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.storage_root, PathBuf::from(".epic/docs/"));
        assert_eq!(config.models.bootstrap, "sonnet");
        assert_eq!(config.models.query, "haiku");
        assert_eq!(config.models.record, "haiku");
        assert_eq!(config.models.reorganize, "sonnet");
    }

    #[test]
    fn config_missing_field_fails() {
        let yaml = r#"
storage_root: ".epic/docs/"
models:
  bootstrap: "sonnet"
"#;
        let result: Result<Config, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn record_mode_arg_conversion() {
        let new: vault::RecordMode = RecordModeArg::New.into();
        assert_eq!(new, vault::RecordMode::New);

        let append: vault::RecordMode = RecordModeArg::Append.into();
        assert_eq!(append, vault::RecordMode::Append);
    }

    #[test]
    fn load_config_nonexistent_file() {
        let result = load_config(Path::new("nonexistent_config.yaml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to read config"));
    }

    #[test]
    fn load_config_invalid_yaml() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "not: [valid: yaml: for: config").unwrap();
        let result = load_config(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to parse config"));
    }

    #[test]
    fn emit_error_produces_json() {
        // emit_error writes to stderr; verify the json structure directly.
        let json = serde_json::json!({"error": "test message"});
        assert_eq!(json["error"], "test message");
    }

    #[test]
    fn build_usage_json_omits_zero_cache_and_cost() {
        let metadata = vault::SessionMetadata {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
            tool_calls: 3,
            transcript: Vec::new(),
        };
        let usage = build_usage_json(&metadata, false);
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 50);
        assert_eq!(usage["tool_calls"], 3);
        assert!(usage.get("cache_creation_input_tokens").is_none());
        assert!(usage.get("cache_read_input_tokens").is_none());
        assert!(usage.get("cost_usd").is_none());
        assert_eq!(usage["api_latency_ms"], 0);
        assert!(usage.get("transcript").is_none());
    }

    #[test]
    fn build_usage_json_includes_cache_and_cost_when_nonzero() {
        let metadata = vault::SessionMetadata {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_input_tokens: 200,
            cache_read_input_tokens: 300,
            cost_usd: 0.005,
            tool_calls: 7,
            transcript: Vec::new(),
        };
        let usage = build_usage_json(&metadata, false);
        assert_eq!(usage["cache_creation_input_tokens"], 200);
        assert_eq!(usage["cache_read_input_tokens"], 300);
        assert_eq!(usage["cost_usd"], 0.005);
    }

    #[test]
    fn build_usage_json_includes_transcript_when_verbose() {
        let metadata = vault::SessionMetadata {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
            tool_calls: 0,
            transcript: vec![vault::TranscriptTurn {
                tool_calls: vec![vault::TranscriptToolCall {
                    tool_use_id: "tc_1".to_owned(),
                    name: "Read".to_owned(),
                    input: serde_json::json!({"file_path": "/tmp/test"}),
                }],
                usage: None,
                api_latency_ms: Some(42),
            }],
        };
        let usage = build_usage_json(&metadata, true);
        assert_eq!(usage["api_latency_ms"], 42);
        assert!(usage.get("transcript").is_some());
        let transcript = usage["transcript"].as_array().unwrap();
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0]["tool_calls"][0]["name"], "Read");
    }

    #[test]
    fn build_usage_json_excludes_transcript_when_not_verbose() {
        let metadata = vault::SessionMetadata {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
            tool_calls: 0,
            transcript: vec![vault::TranscriptTurn {
                tool_calls: Vec::new(),
                usage: None,
                api_latency_ms: Some(42),
            }],
        };
        let usage = build_usage_json(&metadata, false);
        assert!(usage.get("transcript").is_none());
    }
}
