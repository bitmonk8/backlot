use std::io::{BufRead, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use flick::agent;
use flick::config::Config;
use flick::context::Context;
use flick::credential::CredentialStore;
use flick::error::FlickError;
use flick::event::{EventEmitter, JsonLinesEmitter, RawEmitter, Event};
use flick::model::ReasoningLevel;
use flick::provider::{DynProvider, create_provider};
use flick::sandbox;
use flick::tool::ToolRegistry;

#[derive(Parser)]
#[command(name = "flick", version, about = "Ultra-small LLM agent CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Query a model, emit events to stdout
    Run {
        /// Path to TOML config file
        #[arg(long)]
        config: PathBuf,

        /// Query text (or pipe via stdin)
        #[arg(long)]
        query: Option<String>,

        /// Path to JSON context file with prior messages
        #[arg(long)]
        context: Option<PathBuf>,

        /// Plain text output instead of JSON-lines
        #[arg(long)]
        raw: bool,

        /// Dump API request as JSON, no model call
        #[arg(long)]
        dry_run: bool,

        /// Override model ID from config
        #[arg(long)]
        model: Option<String>,

        /// Override reasoning level (minimal, low, medium, high)
        #[arg(long)]
        reasoning: Option<ReasoningLevel>,
    },
    /// Interactive credential setup for a provider
    Setup {
        /// Provider name to configure
        provider: String,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    let (result, raw) = match cli.command {
        Commands::Run {
            config,
            query,
            context,
            raw,
            dry_run,
            model,
            reasoning,
        } => {
            let r = cmd_run(config, query, context, raw, dry_run, model, reasoning).await;
            (r, raw)
        }
        Commands::Setup { provider } => (cmd_setup(&provider).await, false),
    };

    if let Err(e) = result {
        let stderr = std::io::stderr().lock();
        let error_event = Event::Error {
            message: e.to_string(),
            code: e.code().to_string(),
            fatal: true,
        };
        if raw {
            let mut emitter = RawEmitter::new(stderr);
            emitter.emit(&error_event);
        } else {
            let mut emitter = JsonLinesEmitter::new(stderr);
            emitter.emit(&error_event);
        }
        std::process::exit(1);
    }
}

/// Thin wrapper: loads config, credentials, context from filesystem and stdin,
/// then delegates to `cmd_run_core`.
async fn cmd_run(
    config_path: PathBuf,
    query: Option<String>,
    context_path: Option<PathBuf>,
    raw: bool,
    dry_run: bool,
    model_override: Option<String>,
    reasoning_override: Option<ReasoningLevel>,
) -> Result<(), FlickError> {
    let mut config = Config::load(&config_path).await?;

    if let Some(m) = model_override {
        config.override_model_name(m)?;
    }
    if let Some(r) = reasoning_override {
        config.override_reasoning(flick::config::ReasoningConfig { level: r })?;
    }

    let provider_config = config.active_provider()?;
    let cred_store = CredentialStore::new()?;
    let cred_name = provider_config
        .credential
        .as_deref()
        .unwrap_or_else(|| config.model().provider());
    let api_key = cred_store.get(cred_name).await?;
    let provider = create_provider(provider_config, api_key);

    let mut policy_file_path = String::new();
    let tools = match config.sandbox() {
        Some(sandbox_cfg) => {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(|e| FlickError::Sandbox(format!("cannot determine working directory: {e}")))?;

            if let Some(pf) = sandbox_cfg.policy_file() {
                policy_file_path = sandbox::expand_placeholders(pf, &cwd, "", "");
            }

            let expanded_wrapper0 = sandbox::expand_placeholders(
                &sandbox_cfg.wrapper()[0], &cwd, "", &policy_file_path,
            );
            sandbox::validate_wrapper(&expanded_wrapper0)
                .map_err(FlickError::Sandbox)?;

            if let (Some(_), Some(pt)) = (sandbox_cfg.policy_file(), sandbox_cfg.policy_template()) {
                let content = sandbox::generate_policy_content(
                    pt,
                    sandbox_cfg.policy_read_rule(),
                    sandbox_cfg.policy_read_write_rule(),
                    config.resources(),
                    &cwd,
                    &policy_file_path,
                );
                sandbox::write_policy_file(std::path::Path::new(&policy_file_path), &content)
                    .map_err(|e| FlickError::Sandbox(format!("failed to write policy file: {e}")))?;
            }
            let prefix = sandbox::build_prefix(
                sandbox_cfg,
                config.resources(),
                &cwd,
                &policy_file_path,
            );
            let runner = sandbox::SandboxCommandRunner::new(prefix);
            ToolRegistry::from_config_with_runner(
                config.tools(),
                config.resources().to_vec(),
                Box::new(runner),
            )
        }
        _ => ToolRegistry::from_config(config.tools(), config.resources().to_vec()),
    };

    let context = if let Some(ctx_path) = context_path {
        Context::load_from_file(&ctx_path).await?
    } else {
        Context::default()
    };

    let query_text = match query {
        Some(q) => q,
        None => read_stdin().await?,
    };

    let mode = match (raw, dry_run) {
        (true, true) => {
            eprintln!("warning: --dry-run overrides --raw");
            RunMode::DryRun
        }
        (_, true) => RunMode::DryRun,
        (true, false) => RunMode::Raw,
        (false, false) => RunMode::Json,
    };
    // stdout lock held across async agent loop — safe on current_thread runtime
    // only; would need restructuring if runtime flavor changes.
    let stdout = std::io::stdout().lock();
    let result = cmd_run_core(&config, &provider, &tools, context, &query_text, mode, stdout).await;
    if !policy_file_path.is_empty() {
        let _ = std::fs::remove_file(&policy_file_path);
    }
    result
}

enum RunMode {
    Json,
    Raw,
    DryRun,
}

/// Testable core: all dependencies injected, no direct I/O.
async fn cmd_run_core(
    config: &Config,
    provider: &dyn DynProvider,
    tools: &ToolRegistry,
    mut context: Context,
    query: &str,
    mode: RunMode,
    output: impl Write,
) -> Result<(), FlickError> {
    if query.is_empty() {
        return Err(FlickError::NoQuery);
    }
    context.push_user_text(query)?;

    if matches!(mode, RunMode::DryRun) {
        let tool_defs = tools.definitions();
        let params = agent::build_params(config, &context.messages, tool_defs);
        let request_json = provider.build_request(params)?;
        let mut out = output;
        let json_str = serde_json::to_string_pretty(&request_json)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        writeln!(out, "{json_str}").map_err(FlickError::Io)?;
        return Ok(());
    }

    let mut emitter: Box<dyn EventEmitter> = if matches!(mode, RunMode::Raw) {
        Box::new(RawEmitter::new(output))
    } else {
        Box::new(JsonLinesEmitter::new(output))
    };

    agent::run(config, provider, tools, &mut context, emitter.as_mut()).await
}

/// Thin wrapper: uses real stdin/stderr and credential store.
async fn cmd_setup(provider_name: &str) -> Result<(), FlickError> {
    let stdin = std::io::stdin().lock();
    let stderr = std::io::stderr().lock();
    let store = CredentialStore::new()?;
    cmd_setup_core(provider_name, stdin, stderr, &store).await
}

/// Testable core: I/O injected via BufRead/Write, credential store passed in.
async fn cmd_setup_core(
    provider_name: &str,
    mut input: impl BufRead,
    mut output: impl Write,
    store: &CredentialStore,
) -> Result<(), FlickError> {
    let provider_name = provider_name.trim();
    if provider_name.is_empty()
        || provider_name == "."
        || provider_name == ".."
        || provider_name.contains(std::path::MAIN_SEPARATOR)
        || provider_name.contains('/')
        || provider_name.bytes().any(|b| b < 0x20)
    {
        return Err(FlickError::Config(
            flick::error::ConfigError::UnknownProvider(provider_name.to_string()),
        ));
    }
    writeln!(output, "Enter API key for '{provider_name}':").map_err(FlickError::Io)?;
    let mut key = String::new();
    input.read_line(&mut key).map_err(FlickError::Io)?;
    let key = key.trim();
    if key.is_empty() {
        writeln!(output, "No key provided, aborting.").map_err(FlickError::Io)?;
        return Err(FlickError::Io(std::io::Error::other("setup aborted: no key provided")));
    }

    store.set(provider_name, key).await?;
    writeln!(output, "Credential stored for '{provider_name}'.").map_err(FlickError::Io)?;
    Ok(())
}

async fn read_stdin() -> Result<String, FlickError> {
    tokio::task::spawn_blocking(|| {
        use std::io::{IsTerminal, Read};
        if std::io::stdin().is_terminal() {
            return Err(FlickError::NoQuery);
        }
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf.trim().to_string())
    })
    .await
    .map_err(|e| FlickError::Io(std::io::Error::other(e)))?
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    // -- cmd_setup_core tests --

    #[tokio::test]
    async fn setup_core_stores_credential() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-test-key-123\n";
        let mut output = Vec::new();

        cmd_setup_core("anthropic", &input[..], &mut output, &store).await.expect("setup_core");

        let output_str = String::from_utf8(output).expect("utf8");
        assert!(output_str.contains("Enter API key"));
        assert!(output_str.contains("Credential stored"));

        // Verify credential was persisted
        let store2 = CredentialStore::with_dir(dir.path().to_path_buf());
        assert_eq!(store2.get("anthropic").await.expect("get"), "sk-test-key-123");
    }

    #[tokio::test]
    async fn setup_core_rejects_forward_slash() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-key\n";
        let mut output = Vec::new();

        let result = cmd_setup_core("my/provider", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "forward slash in provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_platform_separator() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-key\n";
        let mut output = Vec::new();

        let name = format!("my{}provider", std::path::MAIN_SEPARATOR);
        let result = cmd_setup_core(&name, &input[..], &mut output, &store).await;
        assert!(result.is_err(), "path separator in provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_empty_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-key\n";
        let mut output = Vec::new();

        let result = cmd_setup_core("", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "empty provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_whitespace_only_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-key\n";
        let mut output = Vec::new();

        let result = cmd_setup_core("  ", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "whitespace-only provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_control_characters() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-key\n";
        let mut output = Vec::new();

        let result = cmd_setup_core("my\0provider", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "null byte in provider name should be rejected");

        let result = cmd_setup_core("my\nprovider", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "newline in provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_dot_traversal() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"sk-key\n";
        let mut output = Vec::new();

        let result = cmd_setup_core(".", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "'.' as provider name should be rejected");

        let result = cmd_setup_core("..", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "'..' as provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_empty_key_aborts() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let input = b"\n";
        let mut output = Vec::new();

        let result = cmd_setup_core("test", &input[..], &mut output, &store).await;
        assert!(result.is_err(), "empty key should return error");

        let output_str = String::from_utf8(output).expect("utf8");
        assert!(output_str.contains("No key provided"));
    }

    // -- cmd_run_core tests --

    use flick::provider::{ModelResponse, RequestParams};
    use std::pin::Pin;

    struct StubProvider;
    impl DynProvider for StubProvider {
        fn call_boxed<'a>(
            &'a self,
            _params: RequestParams<'a>,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, flick::error::ProviderError>> + Send + 'a>> {
            Box::pin(async { unreachable!() })
        }
        fn build_request(&self, _params: RequestParams<'_>) -> Result<serde_json::Value, flick::error::ProviderError> {
            Ok(serde_json::json!({"model": "test"}))
        }
    }

    fn stub_config() -> Config {
        Config::parse(r#"
[model]
provider = "test"
name = "test-model"
max_tokens = 1024

[provider.test]
api = "messages"
"#).expect("stub config should parse")
    }

    #[tokio::test]
    async fn run_core_empty_query_returns_no_query() {
        let config = stub_config();
        let tools = ToolRegistry::from_config(config.tools(), vec![]);
        let mut output = Vec::new();

        let result = cmd_run_core(&config, &StubProvider, &tools, Context::default(), "", RunMode::Json, &mut output).await;
        assert!(matches!(result, Err(FlickError::NoQuery)));
    }

    #[tokio::test]
    async fn run_core_dry_run_writes_json() {
        let config = stub_config();
        let tools = ToolRegistry::from_config(config.tools(), vec![]);
        let mut output = Vec::new();

        let result = cmd_run_core(&config, &StubProvider, &tools, Context::default(), "hello", RunMode::DryRun, &mut output).await;
        result.expect("should succeed");

        let output_str = String::from_utf8(output).expect("utf8");
        let parsed: serde_json::Value = serde_json::from_str(output_str.trim()).expect("valid JSON");
        assert_eq!(parsed["model"], "test");
    }
}
