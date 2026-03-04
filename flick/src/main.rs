use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use xxhash_rust::xxh3::xxh3_128;

use flick::runner;
use flick::config::Config;
use flick::context::Context;
use flick::credential::CredentialStore;
use flick::error::FlickError;
use flick::history;
use flick::model::ReasoningLevel;
use flick::model_list::{self, ModelFetcher};
use flick::prompter::{Prompter, TerminalPrompter};
use flick::provider::{DynProvider, create_provider};
use flick::result::{FlickResult, ResultError, ResultStatus, UsageSummary};

#[derive(Parser)]
#[command(name = "flick", version, about = "Ultra-small LLM runner CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Query a model, return single JSON result to stdout
    Run {
        /// Path to config file (.yaml, .yml, or .json)
        #[arg(long)]
        config: PathBuf,

        /// Query text (or pipe via stdin)
        #[arg(long)]
        query: Option<String>,

        /// Resume a previous session by context hash
        #[arg(long)]
        resume: Option<String>,

        /// Path to JSON file containing tool results for resumed session
        #[arg(long)]
        tool_results: Option<PathBuf>,

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
    /// List onboarded providers
    List,
    /// Interactive config file generator
    Init {
        /// Output file path (use '-' for stdout)
        #[arg(long, default_value = "flick.yaml")]
        output: PathBuf,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            config,
            query,
            resume,
            tool_results,
            dry_run,
            model,
            reasoning,
        } => {
            if let Err(e) = cmd_run(config, query, resume, tool_results, dry_run, model, reasoning).await {
                let error_result = FlickResult {
                    status: ResultStatus::Error,
                    content: vec![],
                    usage: None,
                    context_hash: None,
                    error: Some(ResultError {
                        message: e.to_string(),
                        code: e.code().to_string(),
                    }),
                };
                if let Ok(json) = serde_json::to_string(&error_result) {
                    println!("{json}");
                }
                std::process::exit(1);
            }
        }
        Commands::Setup { provider } => {
            if let Err(e) = cmd_setup(&provider).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Commands::List => {
            if let Err(e) = cmd_list().await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Init { output } => {
            if let Err(e) = cmd_init(output).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Thin wrapper: loads config, credentials, context from filesystem and stdin,
/// then delegates to `cmd_run_core`.
async fn cmd_run(
    config_path: PathBuf,
    query: Option<String>,
    resume: Option<String>,
    tool_results_path: Option<PathBuf>,
    dry_run: bool,
    model_override: Option<String>,
    reasoning_override: Option<ReasoningLevel>,
) -> Result<(), FlickError> {
    // Validate CLI argument combinations
    validate_run_args(query.as_deref(), resume.as_deref(), tool_results_path.as_deref())?;

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
    let cred_entry = cred_store.get(cred_name).await?;
    let provider = create_provider(provider_config, cred_entry.key, &cred_entry.base_url);


    let (mut context, query_text) = if let Some(ref hash) = resume {
        // Resume session
        let flick_dir = flick::credential::flick_dir()?;
        let context_file = flick_dir.join("contexts").join(format!("{hash}.json"));
        let mut ctx = Context::load_from_file(&context_file).await?;

        // tool_results_path guaranteed by validate_run_args
        let tool_results =
            flick::context::load_tool_results(tool_results_path.as_ref().unwrap()).await?;
        ctx.push_tool_results(tool_results)?;
        (ctx, String::new()) // no user query on resume
    } else {
        // New session
        let qt = match &query {
            Some(q) => q.clone(),
            None => read_stdin().await?,
        };
        (Context::default(), qt)
    };

    let mut stdout = std::io::stdout().lock();
    let flick_result = cmd_run_core(
        &config, &provider, &mut context, &query_text, dry_run, &mut stdout,
    ).await?;

    // For non-dry-run runs, compute context hash, write context file, output result
    if let Some(mut result) = flick_result {
        let flick_dir = flick::credential::flick_dir()?;
        let context_bytes = serde_json::to_vec(&context)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        let hash = xxh3_128(&context_bytes);
        let hash_hex = format!("{hash:032x}");

        // Write context file
        let contexts_dir = flick_dir.join("contexts");
        tokio::fs::create_dir_all(&contexts_dir).await?;
        let context_file = contexts_dir.join(format!("{hash_hex}.json"));
        if !tokio::fs::try_exists(&context_file).await.unwrap_or(false) {
            tokio::fs::write(&context_file, &context_bytes).await?;
        }

        result.context_hash = Some(hash_hex.clone());

        let json = serde_json::to_string(&result)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        writeln!(stdout, "{json}").map_err(FlickError::Io)?;

        // Record history
        let usage = result.usage.unwrap_or(UsageSummary {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
        });
        let invocation = history::Invocation {
            config_path: config_path.clone(),
            model: config.model().name().to_string(),
            provider: config.model().provider().to_string(),
            query: query_text,
            reasoning: config.model().reasoning().map(|r| r.level),
            resume_hash: resume.clone(),
        };
        if let Err(e) = history::record(invocation, &usage, &hash_hex, &flick_dir).await {
            eprintln!("warning: failed to write history: {e}");
        }
    }

    Ok(())
}

/// Validate `flick run` argument combinations.
///
/// - `--resume` and `--tool-results` must both be present or both absent.
/// - `--query` and `--resume` are mutually exclusive.
fn validate_run_args(
    query: Option<&str>,
    resume: Option<&str>,
    tool_results_path: Option<&std::path::Path>,
) -> Result<(), FlickError> {
    if let Some(hash) = resume {
        if hash.len() != 32
            || !hash
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(FlickError::InvalidArguments(
                "--resume hash must be exactly 32 lowercase hex characters".into(),
            ));
        }
    }
    match (resume, tool_results_path) {
        (Some(_), None) => {
            return Err(FlickError::InvalidArguments(
                "--resume requires --tool-results".into(),
            ));
        }
        (None, Some(_)) => {
            return Err(FlickError::InvalidArguments(
                "--tool-results requires --resume".into(),
            ));
        }
        _ => {}
    }
    if query.is_some() && resume.is_some() {
        return Err(FlickError::InvalidArguments(
            "--query and --resume are mutually exclusive".into(),
        ));
    }
    Ok(())
}

/// Testable core: all dependencies injected, no direct I/O.
///
/// Returns `None` for dry-run, `Some(FlickResult)` for real runs.
async fn cmd_run_core(
    config: &Config,
    provider: &dyn DynProvider,
    context: &mut Context,
    query: &str,
    dry_run: bool,
    output: &mut impl Write,
) -> Result<Option<FlickResult>, FlickError> {
    // Push user query only for new sessions (non-empty query)
    if !query.is_empty() {
        context.push_user_text(query)?;
    } else if context.messages.is_empty() {
        return Err(FlickError::NoQuery);
    }

    if dry_run {
        let tool_defs: Vec<flick::provider::ToolDefinition> = config
            .tools()
            .iter()
            .map(flick::config::ToolConfig::to_definition)
            .collect();
        let params = runner::build_params(config, &context.messages, &tool_defs);
        let request_json = provider.build_request(params)?;
        let json_str = serde_json::to_string_pretty(&request_json)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        writeln!(output, "{json_str}").map_err(FlickError::Io)?;
        return Ok(None);
    }

    let result = runner::run(config, provider, context).await?;
    Ok(Some(result))
}

/// Thin wrapper: uses real stdout and credential store.
async fn cmd_list() -> Result<(), FlickError> {
    let store = CredentialStore::new()?;
    let stdout = std::io::stdout().lock();
    cmd_list_core(&store, stdout).await
}

/// Testable core: writes one provider per line to output (tab-separated columns).
async fn cmd_list_core(store: &CredentialStore, mut output: impl Write) -> Result<(), FlickError> {
    let providers = store.list().await?;
    for info in &providers {
        writeln!(output, "{}\t{}\t{}", info.name, info.api, info.base_url)
            .map_err(FlickError::Io)?;
    }
    Ok(())
}

/// Thin wrapper: uses real terminal prompter and credential store.
async fn cmd_setup(provider_name: &str) -> Result<(), FlickError> {
    let prompter = TerminalPrompter::new();
    let store = CredentialStore::new()?;
    cmd_setup_core(provider_name, &prompter, &store).await
}

/// Testable core: prompts injected via Prompter trait, credential store passed in.
async fn cmd_setup_core(
    provider_name: &str,
    prompter: &dyn Prompter,
    store: &CredentialStore,
) -> Result<(), FlickError> {
    let provider_name = provider_name.trim();
    if provider_name.is_empty()
        || provider_name == "."
        || provider_name == ".."
        || provider_name.contains(std::path::MAIN_SEPARATOR)
        || provider_name.contains('/')
        || provider_name.contains('.')
        || provider_name.bytes().any(|b| b < 0x20)
    {
        return Err(FlickError::Config(
            flick::error::ConfigError::UnknownProvider(provider_name.to_string()),
        ));
    }

    let key = prompter.password(&format!("API key for '{provider_name}'"))?;
    if key.trim().is_empty() {
        prompter.message("No key provided, aborting.")?;
        return Err(FlickError::Io(std::io::Error::other("setup aborted: no key provided")));
    }

    // API type: infer for known providers, otherwise prompt
    let api = if provider_name.contains("anthropic") {
        prompter.message("Inferred API type: messages (Anthropic)")?;
        flick::ApiKind::Messages
    } else {
        let items = vec![
            "chat_completions (OpenAI-compatible)".to_string(),
            "messages (Anthropic)".to_string(),
        ];
        let idx = prompter.select("API type", &items, 0)?;
        if idx == 1 {
            flick::ApiKind::Messages
        } else {
            flick::ApiKind::ChatCompletions
        }
    };

    // Base URL: default based on API type
    let default_url = match api {
        flick::ApiKind::Messages => "https://api.anthropic.com",
        flick::ApiKind::ChatCompletions => "https://api.openai.com",
    };
    let base_url = prompter.input("Base URL", Some(default_url))?;

    store.set(provider_name, key.trim(), api, &base_url).await?;
    prompter.message(&format!("Credential stored for '{provider_name}'."))?;
    Ok(())
}

/// Thin wrapper: uses real terminal prompter, credential store, and HTTP model fetcher.
async fn cmd_init(output: PathBuf) -> Result<(), FlickError> {
    let output_str = output.to_string_lossy().to_string();
    let prompter = TerminalPrompter::new();
    let store = CredentialStore::new()?;
    let fetcher = model_list::HttpModelFetcher::new();

    if output_str == "-" {
        let stdout = std::io::stdout().lock();
        cmd_init_core("-", &prompter, &store, &fetcher, stdout).await
    } else {
        use tokio::io::AsyncWriteExt;

        let mut buf = Vec::new();
        cmd_init_core(&output_str, &prompter, &store, &fetcher, &mut buf).await?;
        // Use create_new for atomic fail-if-exists (closes TOCTOU race window
        // between the early check in cmd_init_core and this write).
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    FlickError::Io(std::io::Error::other(format!(
                        "Error: {} already exists. Use a different --output path.",
                        output.display()
                    )))
                } else {
                    FlickError::Io(e)
                }
            })?;
        file.write_all(&buf).await.map_err(FlickError::Io)
    }
}

/// Testable core: all dependencies injected.
async fn cmd_init_core(
    output_path: &str,
    prompter: &dyn Prompter,
    store: &CredentialStore,
    fetcher: &dyn ModelFetcher,
    mut writer: impl Write,
) -> Result<(), FlickError> {
    // Step 0 — File existence check (early abort before interactive flow;
    // cmd_init also uses create_new for atomic race-free creation)
    if output_path != "-" && std::path::Path::new(output_path).exists() {
        return Err(FlickError::Io(std::io::Error::other(format!(
            "Error: {output_path} already exists. Use a different --output path."
        ))));
    }

    // Step 1 — Provider
    let providers = store.list().await?;
    if providers.is_empty() {
        return Err(FlickError::Io(std::io::Error::other(
            "No providers configured. Run 'flick setup <provider>' first.",
        )));
    }

    let provider_items: Vec<String> = providers
        .iter()
        .map(|p| format!("{} ({})", p.name, p.api))
        .collect();
    let provider_idx = prompter.select("Select provider", &provider_items, 0)?;
    let provider_info = &providers[provider_idx];
    let provider_name = &provider_info.name;
    let api = provider_info.api;
    let base_url = &provider_info.base_url;

    // Step 2 — Model
    prompter.message(&format!("Fetching models from {provider_name}..."))?;
    let cred_entry = store.get(provider_name).await?;

    let (model_name, provider_max_tokens) =
        match fetcher.fetch_models(base_url, &cred_entry.key, api).await {
            Ok(models) if !models.is_empty() => {
                let mut items: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
                items.push("(custom)".to_string());
                let model_idx = prompter.select("Select model", &items, 0)?;
                if model_idx == items.len() - 1 {
                    (prompt_model_name(prompter)?, None)
                } else {
                    let selected = &models[model_idx];
                    (selected.id.clone(), selected.max_completion_tokens)
                }
            }
            Ok(_empty) => (prompt_model_name(prompter)?, None),
            Err(e) => {
                prompter.message(&format!(
                    "Could not fetch models from provider: {e}"
                ))?;
                (prompt_model_name(prompter)?, None)
            }
        };

    // Step 3 — Max tokens
    let default_max = provider_max_tokens
        .or_else(|| flick::model::default_max_output_tokens(&model_name))
        .unwrap_or(8192);

    let max_tokens: Option<u32> = match api {
        flick::ApiKind::ChatCompletions => {
            let prompt_label = if provider_max_tokens.is_some() {
                "Max output tokens"
            } else {
                "Max output tokens (enter 'none' to omit)"
            };
            let input = prompter.input(
                prompt_label,
                Some(&default_max.to_string()),
            )?;
            if input.trim().eq_ignore_ascii_case("none") {
                None
            } else {
                Some(parse_max_tokens(&input)?)
            }
        }
        flick::ApiKind::Messages => {
            let input = prompter.input(
                "Max output tokens",
                Some(&default_max.to_string()),
            )?;
            Some(parse_max_tokens(&input)?)
        }
    };

    // Step 4 — System prompt
    let system_input = prompter.input(
        "System prompt",
        Some("You are Flick, a fast LLM runner."),
    )?;
    let system_prompt = {
        let trimmed = system_input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(system_input)
        }
    };

    // Step 5 — Write
    let params = ConfigGenParams {
        provider_name,
        model_name: &model_name,
        max_tokens,
        system_prompt: system_prompt.as_deref(),
        api,
    };
    let yaml_output = generate_config_yaml(&params);

    if output_path != "-" {
        prompter.message(&format!("Writing config to {output_path}"))?;
    }
    writer.write_all(yaml_output.as_bytes()).map_err(FlickError::Io)?;

    Ok(())
}

fn prompt_model_name(prompter: &dyn Prompter) -> Result<String, FlickError> {
    let name = prompter.input("Model name", None)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(FlickError::Io(std::io::Error::other(
            "No model name provided, aborting.",
        )));
    }
    Ok(name)
}

fn parse_max_tokens(input: &str) -> Result<u32, FlickError> {
    let v = input.trim().parse::<u32>().map_err(|_| {
        FlickError::Io(std::io::Error::other(format!(
            "invalid max_tokens value: {input}"
        )))
    })?;
    if v == 0 {
        return Err(FlickError::Io(std::io::Error::other(
            "invalid max_tokens value: must be > 0",
        )));
    }
    Ok(v)
}

struct ConfigGenParams<'a> {
    provider_name: &'a str,
    model_name: &'a str,
    max_tokens: Option<u32>,
    system_prompt: Option<&'a str>,
    api: flick::ApiKind,
}

fn generate_config_yaml(p: &ConfigGenParams<'_>) -> String {
    let mut out = String::new();

    // Header
    out.push_str("# Flick configuration\n");
    out.push_str("# Generated by `flick init`. Edit freely.\n");
    out.push_str("# Reference: docs/CONFIGURATION.md\n");
    out.push('\n');

    // System prompt
    out.push_str("# ── System Prompt (optional) ────────────────────────────────────────\n");
    match p.system_prompt {
        Some(sp) => {
            let _ = writeln!(out, "system_prompt: \"{}\"", yaml_escape(sp));
        }
        None => {
            out.push_str("# system_prompt: \"You are Flick, a fast LLM runner.\"\n");
        }
    }
    out.push('\n');

    // Model section
    out.push_str("# ── Model ────────────────────────────────────────────────────────────\n");
    out.push_str("# provider: must match a key under `provider:` below\n");
    out.push_str("# name:     model identifier\n");
    out.push_str("# max_tokens: maximum output tokens (omit to use model default, Chat\n");
    out.push_str("#   Completions only; Messages API requires a value)\n");
    out.push_str("# temperature: sampling temperature (optional)\n");
    out.push_str("#   Messages API: 0.0–1.0 | Chat Completions: 0.0–2.0\n");
    out.push_str("model:\n");
    let _ = writeln!(out, "  provider: \"{}\"", yaml_escape(p.provider_name));
    let _ = writeln!(out, "  name: \"{}\"", yaml_escape(p.model_name));
    match p.max_tokens {
        Some(v) => { let _ = writeln!(out, "  max_tokens: {v}"); }
        None => out.push_str("  # max_tokens: 8192\n"),
    }
    out.push_str("  # temperature: 0.0\n");
    out.push_str("  # reasoning:\n");
    out.push_str("  #   level: medium  # minimal (1k), low (4k), medium (10k), high (32k)\n");
    out.push('\n');

    // Provider section
    out.push_str("# ── Provider ─────────────────────────────────────────────────────────\n");
    out.push_str("# api: messages (Anthropic) or chat_completions (OpenAI-compatible)\n");
    out.push_str("# credential: credential store key (defaults to provider name)\n");
    out.push_str("provider:\n");
    let _ = writeln!(out, "  \"{}\":", yaml_escape(p.provider_name));
    let _ = writeln!(out, "    api: \"{}\"", yaml_escape(&p.api.to_string()));
    let _ = writeln!(out, "    # credential: \"{}\"", yaml_escape(p.provider_name));
    out.push_str("    # compat:\n");
    out.push_str("    #   explicit_tool_choice_auto: false\n");
    out.push('\n');

    // Tools section (commented-out template)
    out.push_str("# ── Tools (optional) ────────────────────────────────────────────────\n");
    out.push_str("# Declare tool schemas. Flick sends these to the model but does not\n");
    out.push_str("# execute tools — the caller handles execution.\n");
    out.push_str("# tools:\n");
    out.push_str("#   - name: tool_name\n");
    out.push_str("#     description: \"What this tool does\"\n");
    out.push_str("#     parameters:\n");
    out.push_str("#       type: object\n");
    out.push_str("#       properties:\n");
    out.push_str("#         arg:\n");
    out.push_str("#           type: string\n");
    out.push_str("#       required: [arg]\n");
    out.push('\n');

    // Pricing
    out.push_str("# ── Pricing (optional) ──────────────────────────────────────────────\n");
    out.push_str("# Overrides builtin model pricing. Omit to use registry defaults.\n");
    out.push_str("# pricing:\n");
    out.push_str("#   input_per_million: 3.0\n");
    out.push_str("#   output_per_million: 15.0\n");

    out
}

/// Escape a string for use inside a YAML double-quoted scalar.
fn yaml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x00'..='\x08' | '\x0B'..='\x0C' | '\x0E'..='\x1F' | '\x7F' => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            _ => out.push(c),
        }
    }
    out
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use flick::prompter::MockPrompter;

    // -- validate_run_args tests --

    #[test]
    fn validate_resume_without_tool_results_rejected() {
        let result = validate_run_args(
            None,
            Some("a1b2c3d4e5f60718a1b2c3d4e5f60718"),
            None,
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("--resume requires --tool-results"));
    }

    #[test]
    fn validate_tool_results_without_resume_rejected() {
        let result = validate_run_args(
            None,
            None,
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("--tool-results requires --resume"));
    }

    #[test]
    fn validate_query_with_resume_rejected() {
        let result = validate_run_args(
            Some("hello"),
            Some("a1b2c3d4e5f60718a1b2c3d4e5f60718"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn validate_new_session_accepted() {
        validate_run_args(Some("hello"), None, None).unwrap();
    }

    #[test]
    fn validate_resume_session_accepted() {
        validate_run_args(
            None,
            Some("a1b2c3d4e5f60718a1b2c3d4e5f60718"),
            Some(std::path::Path::new("results.json")),
        ).unwrap();
    }

    #[test]
    fn validate_no_args_accepted() {
        validate_run_args(None, None, None).unwrap();
    }

    #[test]
    fn validate_resume_hash_valid_accepted() {
        validate_run_args(
            None,
            Some("00112233445566778899aabbccddeeff"),
            Some(std::path::Path::new("results.json")),
        )
        .unwrap();
    }

    #[test]
    fn validate_resume_hash_path_traversal_rejected() {
        let result = validate_run_args(
            None,
            Some("../../../etc/passwd"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("32 lowercase hex"));
    }

    #[test]
    fn validate_resume_hash_uppercase_rejected() {
        let result = validate_run_args(
            None,
            Some("00112233445566778899AABBCCDDEEFF"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("32 lowercase hex"));
    }

    #[test]
    fn validate_resume_hash_too_short_rejected() {
        let result = validate_run_args(
            None,
            Some("abcdef01"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("32 lowercase hex"));
    }

    #[test]
    fn validate_resume_hash_too_long_rejected() {
        let result = validate_run_args(
            None,
            Some("00112233445566778899aabbccddeeff00"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("32 lowercase hex"));
    }

    #[test]
    fn validate_resume_hash_non_hex_rejected() {
        let result = validate_run_args(
            None,
            Some("00112233445566778899aabbccddeefg"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("32 lowercase hex"));
    }

    // -- cmd_setup_core tests --

    #[tokio::test]
    async fn setup_core_stores_credential_anthropic() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        // Anthropic provider: password + base URL input (API type inferred)
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-test-key-123".into()])
            .with_inputs(vec!["https://api.anthropic.com".into()]);

        cmd_setup_core("anthropic", &prompter, &store).await.expect("setup_core");

        let messages = prompter.collected_messages();
        assert!(messages.iter().any(|m| m.contains("Credential stored")));
        assert!(messages.iter().any(|m| m.contains("Inferred API type")));

        let entry = store.get("anthropic").await.expect("get");
        assert_eq!(entry.key, "sk-test-key-123");
        assert_eq!(entry.api, flick::ApiKind::Messages);
        assert_eq!(entry.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn setup_core_stores_credential_openai() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        // Non-anthropic provider: password + select API type + base URL input
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-openai-key".into()])
            .with_selects(vec![0]) // chat_completions
            .with_inputs(vec!["https://api.openai.com".into()]);

        cmd_setup_core("openai", &prompter, &store).await.expect("setup_core");

        let entry = store.get("openai").await.expect("get");
        assert_eq!(entry.key, "sk-openai-key");
        assert_eq!(entry.api, flick::ApiKind::ChatCompletions);
        assert_eq!(entry.base_url, "https://api.openai.com");
    }

    #[tokio::test]
    async fn setup_core_custom_base_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()])
            .with_selects(vec![1]) // messages
            .with_inputs(vec!["http://proxy:4000".into()]);

        cmd_setup_core("litellm", &prompter, &store).await.expect("setup_core");

        let entry = store.get("litellm").await.expect("get");
        assert_eq!(entry.api, flick::ApiKind::Messages);
        assert_eq!(entry.base_url, "http://proxy:4000");
    }

    #[tokio::test]
    async fn setup_core_rejects_forward_slash() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let result = cmd_setup_core("my/provider", &prompter, &store).await;
        assert!(result.is_err(), "forward slash in provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_platform_separator() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let name = format!("my{}provider", std::path::MAIN_SEPARATOR);
        let result = cmd_setup_core(&name, &prompter, &store).await;
        assert!(result.is_err(), "path separator in provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_empty_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let result = cmd_setup_core("", &prompter, &store).await;
        assert!(result.is_err(), "empty provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_whitespace_only_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let result = cmd_setup_core("  ", &prompter, &store).await;
        assert!(result.is_err(), "whitespace-only provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_control_characters() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let result = cmd_setup_core("my\0provider", &prompter, &store).await;
        assert!(result.is_err(), "null byte in provider name should be rejected");

        let result = cmd_setup_core("my\nprovider", &prompter, &store).await;
        assert!(result.is_err(), "newline in provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_rejects_dot_traversal() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let result = cmd_setup_core(".", &prompter, &store).await;
        assert!(result.is_err(), "'.' as provider name should be rejected");

        let result = cmd_setup_core("..", &prompter, &store).await;
        assert!(result.is_err(), "'..' as provider name should be rejected");
    }

    #[tokio::test]
    async fn setup_core_empty_key_aborts() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec![String::new()]);

        let result = cmd_setup_core("test", &prompter, &store).await;
        assert!(result.is_err(), "empty key should return error");

        let messages = prompter.collected_messages();
        assert!(messages.iter().any(|m| m.contains("No key provided")));
    }

    // -- cmd_list_core tests --

    #[tokio::test]
    async fn list_core_outputs_tab_separated_columns() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("anthropic", "k1", flick::ApiKind::Messages, "https://api.anthropic.com").await.expect("set");
        store.set("openai", "k2", flick::ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set");

        let mut output = Vec::new();
        cmd_list_core(&store, &mut output).await.expect("list_core");

        let text = String::from_utf8(output).expect("utf8");
        assert_eq!(
            text,
            "anthropic\tmessages\thttps://api.anthropic.com\nopenai\tchat_completions\thttps://api.openai.com\n"
        );
    }

    #[tokio::test]
    async fn list_core_empty_produces_no_output() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        let mut output = Vec::new();
        cmd_list_core(&store, &mut output).await.expect("list_core");

        assert!(output.is_empty());
    }

    // -- cmd_run_core tests --

    use flick::context::ContentBlock;
    use flick::provider::{ModelResponse, RequestParams, ToolCallResponse, UsageResponse};
    use flick::result::ResultStatus;
    use std::pin::Pin;
    use std::sync::Mutex;

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

    /// Provider that returns a single canned response, for `cmd_run_core` tests.
    struct InlineTestProvider {
        response: Mutex<Option<ModelResponse>>,
    }

    impl InlineTestProvider {
        fn with_text(text: &str) -> Self {
            Self {
                response: Mutex::new(Some(ModelResponse {
                    text: Some(text.to_string()),
                    thinking: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: UsageResponse::default(),
                })),
            }
        }

        fn with_tool_calls(calls: Vec<ToolCallResponse>) -> Self {
            Self {
                response: Mutex::new(Some(ModelResponse {
                    text: None,
                    thinking: Vec::new(),
                    tool_calls: calls,
                    usage: UsageResponse::default(),
                })),
            }
        }
    }

    impl DynProvider for InlineTestProvider {
        fn call_boxed<'a>(
            &'a self,
            _params: RequestParams<'a>,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<ModelResponse, flick::error::ProviderError>> + Send + 'a>> {
            let response = self
                .response
                .lock()
                .expect("mutex poisoned")
                .take()
                .expect("InlineTestProvider called more than once");
            Box::pin(async move { Ok(response) })
        }

        fn build_request(
            &self,
            _params: RequestParams<'_>,
        ) -> Result<serde_json::Value, flick::error::ProviderError> {
            Ok(serde_json::json!({"model": "test"}))
        }
    }

    fn stub_config() -> Config {
        Config::parse_yaml(r"
model:
  provider: test
  name: test-model
  max_tokens: 1024

provider:
  test:
    api: messages
").expect("stub config should parse")
    }

    #[tokio::test]
    async fn run_core_empty_query_returns_no_query() {
        let config = stub_config();
        let mut output = Vec::new();

        let result = cmd_run_core(&config, &StubProvider, &mut Context::default(), "", false, &mut output).await;
        assert!(matches!(result, Err(FlickError::NoQuery)));
    }

    #[tokio::test]
    async fn run_core_dry_run_writes_json() {
        let config = stub_config();
        let mut output = Vec::new();

        let result = cmd_run_core(&config, &StubProvider, &mut Context::default(), "hello", true, &mut output).await;
        let flick_result = result.expect("should succeed");
        assert!(flick_result.is_none(), "dry-run should return None");

        let output_str = String::from_utf8(output).expect("utf8");
        let parsed: serde_json::Value = serde_json::from_str(output_str.trim()).expect("valid JSON");
        assert_eq!(parsed["model"], "test");
    }

    #[tokio::test]
    async fn run_core_non_dry_run_text_response() {
        let config = stub_config();
        let provider = InlineTestProvider::with_text("Hello from model");
        let mut context = Context::default();
        let mut output = Vec::new();

        let result = cmd_run_core(
            &config, &provider, &mut context, "say hello", false, &mut output,
        )
        .await
        .expect("should succeed");

        let flick_result = result.expect("non-dry-run should return Some");
        assert_eq!(flick_result.status, ResultStatus::Complete);
        assert!(flick_result.content.iter().any(
            |b| matches!(b, ContentBlock::Text { text } if text == "Hello from model")
        ));
    }

    #[tokio::test]
    async fn run_core_non_dry_run_tool_calls() {
        let config = Config::parse_yaml(
            r"
model:
  provider: test
  name: test-model
  max_tokens: 1024

provider:
  test:
    api: messages

tools:
  - name: read_file
    description: Read a file
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
",
        )
        .expect("config should parse");

        let provider = InlineTestProvider::with_tool_calls(vec![ToolCallResponse {
            call_id: "tc_1".into(),
            tool_name: "read_file".into(),
            arguments: r#"{"path":"/tmp/test"}"#.into(),
        }]);
        let mut context = Context::default();
        let mut output = Vec::new();

        let result = cmd_run_core(
            &config, &provider, &mut context, "read the file", false, &mut output,
        )
        .await
        .expect("should succeed");

        let flick_result = result.expect("non-dry-run should return Some");
        assert_eq!(flick_result.status, ResultStatus::ToolCallsPending);
        let tool_use_count = flick_result
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .count();
        assert_eq!(tool_use_count, 1);
    }

    #[tokio::test]
    async fn run_core_resume_path() {
        let config = Config::parse_yaml(
            r"
model:
  provider: test
  name: test-model
  max_tokens: 1024

provider:
  test:
    api: messages

tools:
  - name: read_file
    description: Read a file
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
",
        )
        .expect("config should parse");

        // Build a context as if a previous run returned tool calls and
        // the caller already pushed tool results (simulating --resume).
        let mut context = Context::default();
        context.push_user_text("read the file").expect("push user");
        context
            .push_assistant(vec![ContentBlock::ToolUse {
                id: "tc_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test"}),
            }])
            .expect("push assistant");
        context
            .push_tool_results(vec![ContentBlock::ToolResult {
                tool_use_id: "tc_1".into(),
                content: "file contents here".into(),
                is_error: false,
            }])
            .expect("push tool results");

        let provider = InlineTestProvider::with_text("The file contains data.");
        let mut output = Vec::new();

        // Empty query simulates resume path (tool results already pushed).
        let result = cmd_run_core(
            &config, &provider, &mut context, "", false, &mut output,
        )
        .await
        .expect("should succeed");

        let flick_result = result.expect("non-dry-run should return Some");
        assert_eq!(flick_result.status, ResultStatus::Complete);
        // Context should now have 4 messages: user, assistant(tool_use), user(tool_result), assistant(text)
        assert_eq!(context.messages.len(), 4);
    }

    // -- cmd_init_core tests --

    use flick::model_list::{FetchedModel, MockModelFetcher};

    /// Helper: create a store with one anthropic (messages) provider.
    async fn init_store_with_anthropic(dir: &std::path::Path) -> CredentialStore {
        let store = CredentialStore::with_dir(dir.to_path_buf());
        store
            .set("anthropic", "sk-ant-key", flick::ApiKind::Messages, "https://api.anthropic.com")
            .await
            .expect("set anthropic");
        store
    }

    #[tokio::test]
    async fn init_core_no_providers_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let fetcher = MockModelFetcher::with_models(vec![]);
        let prompter = MockPrompter::new();
        let mut output = Vec::new();

        let result = cmd_init_core("-", &prompter, &store, &fetcher, &mut output).await;
        assert!(result.is_err());
        let err_msg = result.expect_err("should be error").to_string();
        assert!(err_msg.contains("No providers configured"));
    }

    #[tokio::test]
    async fn init_core_basic_messages() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        // Prompts in order:
        // select provider (0), select model (0), input max_tokens ("64000"),
        // input system_prompt (default)
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("provider: \"anthropic\""));
        assert!(text.contains("name: \"claude-sonnet-4-20250514\""));
        assert!(text.contains("max_tokens: 64000"));
        assert!(text.contains("api: \"messages\""));
        // Tool section should be a commented-out template
        assert!(text.contains("# tools:"));
        assert!(text.contains("#   - name: tool_name"));
    }

    #[tokio::test]
    async fn init_core_multi_provider_select() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("anthropic", "k1", flick::ApiKind::Messages, "https://api.anthropic.com").await.expect("set");
        store.set("openai", "k2", flick::ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set");

        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "gpt-4o".into(),
            max_completion_tokens: Some(16_384),
        }]);

        // Select second provider (index 1 = openai), select model (0),
        // input max_tokens, input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![1, 0])
            .with_inputs(vec!["16384".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("provider: \"openai\""));
        assert!(text.contains("api: \"chat_completions\""));
    }

    #[tokio::test]
    async fn init_core_model_fetch_fails_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_error(
            FlickError::Io(std::io::Error::other("network error")),
        );

        // select provider (0), input model name, input max_tokens, input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0])
            .with_inputs(vec!["custom-model".into(), "8192".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("name: \"custom-model\""));
        let messages = prompter.collected_messages();
        assert!(messages.iter().any(|m| m.contains("Could not fetch models")));
    }

    #[tokio::test]
    async fn init_core_custom_model_selection() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        // select provider (0), select model (1 = "(custom)"), input model name,
        // input max_tokens, input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 1])
            .with_inputs(vec!["my-model".into(), "8192".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("name: \"my-model\""));
    }

    #[tokio::test]
    async fn init_core_empty_custom_model_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        // select provider (0), select model (1 = "(custom)"), input empty model name
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 1])
            .with_inputs(vec![String::new()]);

        let mut output = Vec::new();
        let result = cmd_init_core("-", &prompter, &store, &fetcher, &mut output).await;
        assert!(result.is_err());
        let err_msg = result.expect_err("should be error").to_string();
        assert!(err_msg.contains("No model name provided"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_from_provider_metadata() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(32_000),
        }]);

        // select provider (0), select model (0), input max_tokens ("32000"),
        // input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["32000".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("max_tokens: 32000"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_registry_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        // Model matches builtin registry but no max_completion_tokens from provider
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: None,
        }]);

        // Registry default for claude-sonnet-4-20250514 is 64000
        // select provider (0), select model (0), input max_tokens ("64000"),
        // input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("max_tokens: 64000"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_hardcoded_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        // Unknown model, no metadata
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "unknown-model-xyz".into(),
            max_completion_tokens: None,
        }]);

        // Hardcoded fallback is 8192
        // select provider (0), select model (0), input max_tokens ("8192"),
        // input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["8192".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("max_tokens: 8192"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_none_chat_completions() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("openai", "sk-key", flick::ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set");

        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "gpt-4o".into(),
            max_completion_tokens: Some(16_384),
        }]);

        // select provider (0), select model (0), input "none",
        // input system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["none".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("# max_tokens: 8192"));
        assert!(!text.contains("\n  max_tokens:"));
    }

    #[tokio::test]
    async fn init_core_system_prompt_with_quotes() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input system prompt with quotes
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec![
                "64000".into(),
                "You are \"Flick\", a fast runner.".into(),
            ]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("system_prompt: \"You are \\\"Flick\\\", a fast runner.\""));
    }

    #[tokio::test]
    async fn init_core_system_prompt_none() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input "none" for system prompt
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "none".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("# system_prompt: "));
        assert!(!text.contains("\nsystem_prompt: "));
    }

    #[tokio::test]
    async fn init_core_file_exists_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![]);
        let prompter = MockPrompter::new();

        let existing_file = dir.path().join("flick.yaml");
        std::fs::write(&existing_file, "existing").expect("write file");

        let mut output = Vec::new();
        let result = cmd_init_core(
            existing_file.to_str().expect("path str"),
            &prompter,
            &store,
            &fetcher,
            &mut output,
        )
        .await;

        assert!(result.is_err());
        let err_msg = result.expect_err("should be error").to_string();
        assert!(err_msg.contains("already exists"));
    }

    #[tokio::test]
    async fn init_core_stdout_mode() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        // Verify no "Writing config" message for stdout mode
        let messages = prompter.collected_messages();
        assert!(!messages.iter().any(|m| m.contains("Writing config")));

        // Verify output was written
        let text = String::from_utf8(output).expect("utf8");
        assert!(!text.is_empty());
        assert!(text.contains("model:"));
    }

    #[tokio::test]
    async fn init_core_nondefault_base_url_absent_from_yaml() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("litellm", "sk-key", flick::ApiKind::Messages, "http://custom:4000").await.expect("set");

        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(!text.contains("base_url"), "base_url should not appear in generated config");
    }

    #[test]
    fn generate_config_yaml_round_trip_messages() {
        let params = ConfigGenParams {
            provider_name: "anthropic",
            model_name: "claude-sonnet-4-20250514",
            max_tokens: Some(64_000),
            system_prompt: Some("You are Flick, a fast LLM runner."),
            api: flick::ApiKind::Messages,
        };
        let yaml_str = generate_config_yaml(&params);
        assert!(!yaml_str.contains("base_url"), "base_url should not appear in generated config");
        assert!(yaml_str.contains("provider: \"anthropic\""));
        assert!(yaml_str.contains("name: \"claude-sonnet-4-20250514\""));
        assert!(yaml_str.contains("max_tokens: 64000"));
        // Tool section should be commented-out template
        assert!(yaml_str.contains("# tools:"));
        assert!(yaml_str.contains("#   - name: tool_name"));
        // Round-trip: generated YAML should parse
        let _config = Config::parse_yaml(&yaml_str).expect("generated YAML should parse");
    }

    #[tokio::test]
    async fn init_core_default_base_url_absent_from_yaml() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(!text.contains("base_url"), "base_url should not appear in generated config");
    }

    // -- Test gap #11: system prompt with newlines --

    #[tokio::test]
    async fn init_core_system_prompt_with_newlines() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec![
                "64000".into(),
                "Line one\nLine two\nLine three".into(),
            ]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("system_prompt: \"Line one\\nLine two\\nLine three\""));
    }

    // -- Test gap #12: system prompt containing ''' --

    #[tokio::test]
    async fn init_core_system_prompt_with_triple_quotes() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec![
                "64000".into(),
                "Use '''triple quotes''' carefully".into(),
            ]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        // Single quotes don't need escaping in YAML double-quoted strings
        assert!(text.contains("system_prompt: \"Use '''triple quotes''' carefully\""));
    }

    // -- Test gap #13: max_tokens = 0 rejection --

    #[test]
    fn parse_max_tokens_rejects_zero() {
        let result = parse_max_tokens("0");
        assert!(result.is_err());
        let err_msg = result.expect_err("should be error").to_string();
        assert!(err_msg.contains("must be > 0"));
    }

    // -- B5: provider names with dots rejected --

    #[tokio::test]
    async fn setup_core_rejects_dot_in_provider_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let prompter = MockPrompter::new()
            .with_passwords(vec!["sk-key".into()]);

        let result = cmd_setup_core("my.provider", &prompter, &store).await;
        assert!(result.is_err(), "dot in provider name should be rejected");
    }

    // -- C2: round-trip test for Chat Completions with max_tokens = None --

    #[test]
    fn generate_config_yaml_round_trip_chat_completions_no_max_tokens() {
        let params = ConfigGenParams {
            provider_name: "openai",
            model_name: "gpt-4o",
            max_tokens: None,
            system_prompt: Some("You are Flick, a fast LLM runner."),
            api: flick::ApiKind::ChatCompletions,
        };
        let yaml_str = generate_config_yaml(&params);
        // Verify the commented-out max_tokens line is present
        assert!(yaml_str.contains("# max_tokens:"), "expected commented-out max_tokens line");
        // Verify no active max_tokens line
        assert!(!yaml_str.lines().any(|l| {
            let trimmed = l.trim();
            trimmed.starts_with("max_tokens:") && !trimmed.starts_with('#')
        }), "max_tokens should only appear as a comment");
        assert!(yaml_str.contains("provider: \"openai\""));
        assert!(yaml_str.contains("name: \"gpt-4o\""));
        // Round-trip: generated YAML should parse
        let _config = Config::parse_yaml(&yaml_str).expect("generated YAML should parse");
    }
}
