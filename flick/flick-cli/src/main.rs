use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use xxhash_rust::xxh3::xxh3_128;

use flick::FlickClient;
use flick::config::RequestConfig;
use flick::context::Context;
use flick::error::FlickError;
use flick::history;
use flick::model_registry::{self, ModelInfo, ModelRegistry};
use flick::provider_registry::{self, ProviderRegistry};
mod prompter;
use flick::result::{FlickResult, ResultError, ResultStatus, UsageSummary};
use prompter::{Prompter, TerminalPrompter};

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
    },
    /// Manage providers
    #[command(subcommand)]
    Provider(ProviderCommands),
    /// Manage models
    #[command(subcommand)]
    Model(ModelCommands),
    /// Interactive config file generator
    Init {
        /// Output file path (use '-' for stdout)
        #[arg(long, default_value = "flick.yaml")]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum ProviderCommands {
    /// Add or update a provider (interactive)
    Add {
        /// Provider name
        name: String,
    },
    /// List configured providers
    List,
}

#[derive(Subcommand)]
enum ModelCommands {
    /// Add or update a model entry (interactive)
    Add {
        /// Model key name
        name: String,
    },
    /// List configured models
    List,
    /// Remove a model entry
    Remove {
        /// Model key name
        name: String,
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
        } => {
            if let Err(e) = cmd_run(config, query, resume, tool_results, dry_run).await {
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
        Commands::Provider(sub) => {
            let result = match sub {
                ProviderCommands::Add { name } => cmd_provider_add(&name).await,
                ProviderCommands::List => cmd_provider_list().await,
            };
            if let Err(e) = result {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Model(sub) => {
            let result = match sub {
                ModelCommands::Add { name } => cmd_model_add(&name).await,
                ModelCommands::List => cmd_model_list().await,
                ModelCommands::Remove { name } => cmd_model_remove(&name).await,
            };
            if let Err(e) = result {
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

/// Load registries and run.
async fn cmd_run(
    config_path: PathBuf,
    query: Option<String>,
    resume: Option<String>,
    tool_results_path: Option<PathBuf>,
    dry_run: bool,
) -> Result<(), FlickError> {
    validate_run_args(
        query.as_deref(),
        resume.as_deref(),
        tool_results_path.as_deref(),
    )?;

    if let Some(ref q) = query {
        if q.trim().is_empty() {
            return Err(FlickError::NoQuery);
        }
    }

    let request = RequestConfig::load(&config_path).await?;

    let providers = ProviderRegistry::load_default()?;
    let models = ModelRegistry::load_default().await?;

    // Cross-registry validation
    model_registry::validate_registries(&models, &providers).await?;

    let client = FlickClient::new(request, &models, &providers).await?;

    let pricing_zero = client.model_info().input_per_million.is_none()
        && client.model_info().output_per_million.is_none()
        && client.model_info().cache_creation_per_million.is_none()
        && client.model_info().cache_read_per_million.is_none();
    if pricing_zero {
        eprintln!(
            "warning: no pricing info for model '{}'; cost will be reported as 0.0",
            client.model_info().name
        );
    }

    let (mut context, query_text, tool_results) = if let Some(ref hash) = resume {
        let flick_dir = provider_registry::flick_dir()?;
        let context_file = flick_dir.join("contexts").join(format!("{hash}.json"));
        let ctx = Context::load_from_file(&context_file).await?;

        let tr_path = tool_results_path.as_ref().ok_or_else(|| {
            FlickError::InvalidArguments("--resume requires --tool-results".into())
        })?;
        let tr = flick::context::load_tool_results(tr_path).await?;
        (ctx, String::new(), Some(tr))
    } else {
        let qt = match &query {
            Some(q) => q.clone(),
            None => read_stdin().await?,
        };
        (Context::default(), qt, None)
    };

    let mut stdout = std::io::stdout().lock();
    let flick_result = cmd_run_core(
        &client,
        &mut context,
        &query_text,
        tool_results,
        dry_run,
        &mut stdout,
    )
    .await?;

    if let Some(mut result) = flick_result {
        let flick_dir = provider_registry::flick_dir()?;
        let context_bytes =
            serde_json::to_vec(&context).map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        let hash = xxh3_128(&context_bytes);
        let hash_hex = format!("{hash:032x}");

        let contexts_dir = flick_dir.join("contexts");
        tokio::fs::create_dir_all(&contexts_dir).await?;
        let context_file = contexts_dir.join(format!("{hash_hex}.json"));
        if !tokio::fs::try_exists(&context_file).await.unwrap_or(false) {
            tokio::fs::write(&context_file, &context_bytes).await?;
        }

        result.context_hash = Some(hash_hex.clone());

        let json =
            serde_json::to_string(&result).map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        writeln!(stdout, "{json}").map_err(FlickError::Io)?;

        let usage = result.usage.unwrap_or(UsageSummary {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
        });
        let invocation = history::Invocation {
            config_path: config_path.clone(),
            model: client.model_info().name.clone(),
            provider: client.model_info().provider.clone(),
            query: query_text,
            reasoning: client.config().reasoning().map(|r| r.level),
            resume_hash: resume.clone(),
        };
        if let Err(e) = history::record(invocation, &usage, &hash_hex, &flick_dir).await {
            eprintln!("warning: failed to write history: {e}");
        }
    }

    Ok(())
}

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

async fn cmd_run_core(
    client: &FlickClient,
    context: &mut Context,
    query: &str,
    tool_results: Option<Vec<flick::context::ContentBlock>>,
    dry_run: bool,
    output: &mut impl Write,
) -> Result<Option<FlickResult>, FlickError> {
    if dry_run {
        if query.trim().is_empty() {
            return Err(FlickError::NoQuery);
        }
        let request_json = client.build_request(query)?;
        let json_str = serde_json::to_string_pretty(&request_json)
            .map_err(|e| FlickError::Io(std::io::Error::other(e)))?;
        writeln!(output, "{json_str}").map_err(FlickError::Io)?;
        return Ok(None);
    }

    let result = if let Some(tr) = tool_results {
        client.resume(context, tr).await?
    } else if !query.trim().is_empty() {
        client.run(query, context).await?
    } else {
        return Err(FlickError::NoQuery);
    };
    Ok(Some(result))
}

// -- Provider commands --

async fn cmd_provider_add(provider_name: &str) -> Result<(), FlickError> {
    let prompter = TerminalPrompter::new();
    let registry = ProviderRegistry::load_default()?;
    cmd_provider_add_core(provider_name, &prompter, &registry).await
}

async fn cmd_provider_add_core(
    provider_name: &str,
    prompter: &dyn Prompter,
    registry: &ProviderRegistry,
) -> Result<(), FlickError> {
    let provider_name = provider_name.trim();
    if provider_name.is_empty()
        || provider_name.len() > 255
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
        return Err(FlickError::Io(std::io::Error::other(
            "setup aborted: no key provided",
        )));
    }
    if key.trim().len() > 4096 {
        return Err(FlickError::InvalidArguments(
            "API key exceeds 4096 byte limit".into(),
        ));
    }
    if key.trim().bytes().any(|b| b < 0x20 || b == 0x7F) {
        return Err(FlickError::InvalidArguments(
            "API key contains control characters".into(),
        ));
    }

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

    let default_url = match api {
        flick::ApiKind::Messages => "https://api.anthropic.com",
        flick::ApiKind::ChatCompletions => "https://api.openai.com",
    };
    let base_url = prompter.input("Base URL", Some(default_url))?;

    registry
        .set(provider_name, key.trim(), api, &base_url, None)
        .await?;
    prompter.message(&format!("Provider stored for '{provider_name}'."))?;
    Ok(())
}

async fn cmd_provider_list() -> Result<(), FlickError> {
    let registry = ProviderRegistry::load_default()?;
    let stdout = std::io::stdout().lock();
    cmd_provider_list_core(&registry, stdout).await
}

async fn cmd_provider_list_core(
    registry: &ProviderRegistry,
    mut output: impl Write,
) -> Result<(), FlickError> {
    let providers = registry.list().await?;
    for info in &providers {
        writeln!(output, "{}\t{}\t{}", info.name, info.api, info.base_url)
            .map_err(FlickError::Io)?;
    }
    Ok(())
}

// -- Model commands --

async fn cmd_model_add(name: &str) -> Result<(), FlickError> {
    let prompter = TerminalPrompter::new();
    let providers = ProviderRegistry::load_default()?;
    let flick_dir = provider_registry::flick_dir()?;
    let models_path = flick_dir.join("models");
    let mut models = ModelRegistry::load_from_path(&models_path).await?;
    cmd_model_add_core(name, &prompter, &providers, &mut models, &flick_dir).await
}

async fn cmd_model_add_core(
    name: &str,
    prompter: &dyn Prompter,
    providers: &ProviderRegistry,
    models: &mut ModelRegistry,
    flick_dir: &std::path::Path,
) -> Result<(), FlickError> {
    let provider_list = providers.list().await?;
    if provider_list.is_empty() {
        return Err(FlickError::Io(std::io::Error::other(
            "No providers configured. Run 'flick provider add <name>' first.",
        )));
    }

    let provider_items: Vec<String> = provider_list
        .iter()
        .map(|p| format!("{} ({})", p.name, p.api))
        .collect();
    let provider_idx = prompter.select("Select provider", &provider_items, 0)?;
    let provider_name = &provider_list[provider_idx].name;

    let model_id = prompter.input("Model ID (e.g. claude-sonnet-4-6)", None)?;
    let model_id = model_id.trim().to_string();
    if model_id.is_empty() {
        return Err(FlickError::Io(std::io::Error::other(
            "No model ID provided, aborting.",
        )));
    }

    let max_tokens_input =
        prompter.input("Max output tokens (enter 'none' to omit)", Some("8192"))?;
    let max_tokens = if max_tokens_input.trim().eq_ignore_ascii_case("none") {
        None
    } else {
        Some(parse_max_tokens(&max_tokens_input)?)
    };

    let input_price = prompter.input("Input price per million tokens (or 'none')", Some("none"))?;
    let input_per_million = parse_optional_price(&input_price)?;

    let output_price =
        prompter.input("Output price per million tokens (or 'none')", Some("none"))?;
    let output_per_million = parse_optional_price(&output_price)?;

    let cache_creation_price = prompter.input(
        "Cache creation price per million tokens (or 'none')",
        Some("none"),
    )?;
    let cache_creation_per_million = parse_optional_price(&cache_creation_price)?;

    let cache_read_price = prompter.input(
        "Cache read price per million tokens (or 'none')",
        Some("none"),
    )?;
    let cache_read_per_million = parse_optional_price(&cache_read_price)?;

    let info = ModelInfo {
        provider: provider_name.clone(),
        name: model_id,
        max_tokens,
        input_per_million,
        output_per_million,
        cache_creation_per_million,
        cache_read_per_million,
    };

    models.set(name, info, flick_dir).await?;
    prompter.message(&format!("Model '{name}' stored."))?;
    Ok(())
}

async fn cmd_model_list() -> Result<(), FlickError> {
    let flick_dir = provider_registry::flick_dir()?;
    let models_path = flick_dir.join("models");
    let models = ModelRegistry::load_from_path(&models_path).await?;
    let mut stdout = std::io::stdout().lock();
    for (key, info) in models.list() {
        writeln!(
            stdout,
            "{key}\t{}\t{}\t{}",
            info.provider,
            info.name,
            info.max_tokens
                .map_or_else(|| "-".to_string(), |v| v.to_string())
        )
        .map_err(FlickError::Io)?;
    }
    Ok(())
}

async fn cmd_model_remove(name: &str) -> Result<(), FlickError> {
    let flick_dir = provider_registry::flick_dir()?;
    let models_path = flick_dir.join("models");
    let mut models = ModelRegistry::load_from_path(&models_path).await?;
    if models.remove(name, &flick_dir).await? {
        eprintln!("Model '{name}' removed.");
    } else {
        eprintln!("Model '{name}' not found.");
    }
    Ok(())
}

// -- Init command --

async fn cmd_init(output: PathBuf) -> Result<(), FlickError> {
    let output_str = output.to_string_lossy().to_string();
    let prompter = TerminalPrompter::new();
    let flick_dir = provider_registry::flick_dir()?;
    let models_path = flick_dir.join("models");
    let models = ModelRegistry::load_from_path(&models_path).await?;

    if output_str == "-" {
        let stdout = std::io::stdout().lock();
        cmd_init_core("-", &prompter, &models, stdout)
    } else {
        use tokio::io::AsyncWriteExt;

        let mut buf = Vec::new();
        cmd_init_core(&output_str, &prompter, &models, &mut buf)?;
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

fn cmd_init_core(
    output_path: &str,
    prompter: &dyn Prompter,
    models: &ModelRegistry,
    mut writer: impl Write,
) -> Result<(), FlickError> {
    if output_path != "-" && std::path::Path::new(output_path).exists() {
        return Err(FlickError::Io(std::io::Error::other(format!(
            "Error: {output_path} already exists. Use a different --output path."
        ))));
    }

    if models.is_empty() {
        return Err(FlickError::Io(std::io::Error::other(
            "No models configured. Run 'flick model add <name>' first.",
        )));
    }

    let model_entries = models.list();
    let model_items: Vec<String> = model_entries
        .iter()
        .map(|(key, info)| format!("{key} ({}, {})", info.provider, info.name))
        .collect();
    let model_idx = prompter.select("Select model", &model_items, 0)?;
    let (model_key, _model_info) = &model_entries[model_idx];

    let system_input =
        prompter.input("System prompt", Some("You are Flick, a fast LLM runner."))?;
    let system_prompt = {
        let trimmed = system_input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(system_input)
        }
    };

    let yaml_output = generate_config_yaml(model_key, system_prompt.as_deref());

    if output_path != "-" {
        prompter.message(&format!("Writing config to {output_path}"))?;
    }
    writer
        .write_all(yaml_output.as_bytes())
        .map_err(FlickError::Io)?;

    Ok(())
}

fn generate_config_yaml(model_key: &str, system_prompt: Option<&str>) -> String {
    let mut out = String::new();

    out.push_str("# Flick request configuration\n");
    out.push_str("# Generated by `flick init`. Edit freely.\n");
    out.push('\n');

    // Model
    let _ = writeln!(out, "model: \"{}\"", yaml_escape(model_key));
    out.push('\n');

    // System prompt
    match system_prompt {
        Some(sp) => {
            let _ = writeln!(out, "system_prompt: \"{}\"", yaml_escape(sp));
        }
        None => {
            out.push_str("# system_prompt: \"You are Flick, a fast LLM runner.\"\n");
        }
    }
    out.push('\n');

    // Temperature
    out.push_str("# temperature: 0.0\n");
    out.push('\n');

    // Reasoning
    out.push_str("# reasoning:\n");
    out.push_str("#   level: medium  # minimal (1k), low (4k), medium (10k), high (32k)\n");
    out.push('\n');

    // Tools
    out.push_str("# tools:\n");
    out.push_str("#   - name: tool_name\n");
    out.push_str("#     description: \"What this tool does\"\n");
    out.push_str("#     parameters:\n");
    out.push_str("#       type: object\n");
    out.push_str("#       properties:\n");
    out.push_str("#         arg:\n");
    out.push_str("#           type: string\n");
    out.push_str("#       required: [arg]\n");

    out
}

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

fn parse_optional_price(input: &str) -> Result<Option<f64>, FlickError> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let v = trimmed.parse::<f64>().map_err(|_| {
        FlickError::Io(std::io::Error::other(format!(
            "invalid price value: {input}"
        )))
    })?;
    if !v.is_finite() || v < 0.0 {
        return Err(FlickError::Io(std::io::Error::other(
            "price must be non-negative and finite",
        )));
    }
    Ok(Some(v))
}

/// 10 MiB cap — LLM APIs reject larger payloads anyway.
const STDIN_MAX_BYTES: usize = 10 * 1024 * 1024;

async fn read_stdin() -> Result<String, FlickError> {
    tokio::task::spawn_blocking(|| {
        use std::io::{IsTerminal, Read};
        if std::io::stdin().is_terminal() {
            return Err(FlickError::NoQuery);
        }
        let mut buf = String::new();
        std::io::stdin()
            .take(STDIN_MAX_BYTES as u64 + 1)
            .read_to_string(&mut buf)?;
        if buf.len() > STDIN_MAX_BYTES {
            return Err(FlickError::StdinTooLarge(STDIN_MAX_BYTES));
        }
        if buf.trim().is_empty() {
            if buf.is_empty() {
                return Err(FlickError::NoQuery);
            }
            return Err(FlickError::WhitespaceOnlyStdin);
        }
        Ok(buf.trim().to_string())
    })
    .await
    .map_err(|e| FlickError::Io(std::io::Error::other(e)))?
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::prompter::MockPrompter;

    // -- validate_run_args tests --

    #[test]
    fn validate_resume_without_tool_results_rejected() {
        let result = validate_run_args(None, Some("a1b2c3d4e5f60718a1b2c3d4e5f60718"), None);
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("--resume requires --tool-results"));
    }

    #[test]
    fn validate_tool_results_without_resume_rejected() {
        let result = validate_run_args(None, None, Some(std::path::Path::new("results.json")));
        let err = result.unwrap_err();
        assert!(matches!(err, FlickError::InvalidArguments(_)));
        assert!(err.to_string().contains("--tool-results requires --resume"));
    }

    #[test]
    fn validate_resume_and_query_mutually_exclusive() {
        let result = validate_run_args(
            Some("hello"),
            Some("a1b2c3d4e5f60718a1b2c3d4e5f60718"),
            Some(std::path::Path::new("results.json")),
        );
        let err = result.unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn validate_resume_bad_hash_rejected() {
        let result = validate_run_args(
            None,
            Some("BADHASH"),
            Some(std::path::Path::new("results.json")),
        );
        assert!(result.is_err());
    }

    #[test]
    fn validate_all_none_accepted() {
        validate_run_args(None, None, None).unwrap();
    }

    #[test]
    fn validate_query_only_accepted() {
        validate_run_args(Some("hello"), None, None).unwrap();
    }

    // -- provider add tests --

    #[tokio::test]
    async fn provider_add_stores_credential() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());

        let mock = MockPrompter::new()
            .with_passwords(vec!["test-key".into()])
            .with_selects(vec![1]) // messages
            .with_inputs(vec!["https://api.anthropic.com".into()]);

        cmd_provider_add_core("test_provider", &mock, &registry)
            .await
            .expect("should succeed");

        let entry = registry.get("test_provider").await.expect("get");
        assert_eq!(entry.key, "test-key");
        assert_eq!(entry.api, flick::ApiKind::Messages);
    }

    #[tokio::test]
    async fn provider_add_empty_name_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let mock = MockPrompter::new();

        let result = cmd_provider_add_core("", &mock, &registry).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn provider_add_empty_key_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let mock = MockPrompter::new().with_passwords(vec![String::new()]);

        let result = cmd_provider_add_core("test", &mock, &registry).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn provider_add_long_name_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let mock = MockPrompter::new();
        let long_name = "a".repeat(256);

        let result = cmd_provider_add_core(&long_name, &mock, &registry).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn provider_add_max_length_name_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let mock = MockPrompter::new()
            .with_passwords(vec!["test-key".into()])
            .with_selects(vec![0]) // chat_completions
            .with_inputs(vec!["https://api.openai.com".into()]);
        let name_255 = "a".repeat(255);

        cmd_provider_add_core(&name_255, &mock, &registry)
            .await
            .expect("255-char name should be accepted");
    }

    #[tokio::test]
    async fn provider_add_key_with_control_chars_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let mock = MockPrompter::new().with_passwords(vec!["key\x01bad".into()]);

        let result = cmd_provider_add_core("test", &mock, &registry).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("control characters"));
    }

    #[tokio::test]
    async fn provider_add_key_too_long_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let long_key = "k".repeat(4097);
        let mock = MockPrompter::new().with_passwords(vec![long_key]);

        let result = cmd_provider_add_core("test", &mock, &registry).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("4096"));
    }

    // -- provider list tests --

    #[tokio::test]
    async fn provider_list_formats_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        registry
            .set(
                "anthropic",
                "key",
                flick::ApiKind::Messages,
                "https://api.anthropic.com",
                None,
            )
            .await
            .expect("set");

        let mut buf = Vec::new();
        cmd_provider_list_core(&registry, &mut buf)
            .await
            .expect("list");
        let output = String::from_utf8(buf).expect("utf8");
        assert!(output.contains("anthropic"));
        assert!(output.contains("messages"));
    }

    // -- init tests --

    #[tokio::test]
    async fn init_generates_yaml_to_stdout() {
        let mut models = ModelRegistry::empty();
        let dir = tempfile::tempdir().expect("tempdir");
        models
            .set(
                "fast",
                ModelInfo {
                    provider: "anthropic".into(),
                    name: "claude-haiku".into(),
                    max_tokens: Some(8192),
                    input_per_million: None,
                    output_per_million: None,
                    cache_creation_per_million: None,
                    cache_read_per_million: None,
                },
                dir.path(),
            )
            .await
            .expect("set model");

        let mock = MockPrompter::new()
            .with_selects(vec![0])
            .with_inputs(vec!["Test prompt".into()]);

        let mut buf = Vec::new();
        cmd_init_core("-", &mock, &models, &mut buf).expect("init");
        let output = String::from_utf8(buf).expect("utf8");
        assert!(output.contains("model: \"fast\""));
        assert!(output.contains("Test prompt"));
    }

    #[tokio::test]
    async fn init_empty_models_rejected() {
        let models = ModelRegistry::empty();
        let mock = MockPrompter::new();
        let mut buf = Vec::new();
        let result = cmd_init_core("-", &mock, &models, &mut buf);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No models"));
    }

    // -- yaml_escape tests --

    #[test]
    fn yaml_escape_special_chars() {
        assert_eq!(yaml_escape("a\"b"), "a\\\"b");
        assert_eq!(yaml_escape("a\\b"), "a\\\\b");
        assert_eq!(yaml_escape("a\nb"), "a\\nb");
    }

    // -- parse_max_tokens tests --

    #[test]
    fn parse_max_tokens_valid() {
        assert_eq!(parse_max_tokens("4096").unwrap(), 4096);
    }

    #[test]
    fn parse_max_tokens_zero_rejected() {
        assert!(parse_max_tokens("0").is_err());
    }

    #[test]
    fn parse_max_tokens_non_numeric_rejected() {
        assert!(parse_max_tokens("abc").is_err());
    }

    // -- run_core tests --

    #[tokio::test]
    async fn run_core_dry_run() {
        use flick::test_support::SingleShotProvider;

        let config = RequestConfig::parse_yaml("model: test\n").expect("parse");
        let model_info = flick::model_registry::ModelInfo {
            provider: "test".into(),
            name: "test-model".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        };
        let client = FlickClient::new_with_provider(
            config,
            model_info,
            flick::ApiKind::Messages,
            SingleShotProvider::stub(),
        );
        let mut ctx = Context::default();
        let mut buf = Vec::new();
        let result = cmd_run_core(&client, &mut ctx, "hello", None, true, &mut buf)
            .await
            .expect("dry run");
        assert!(result.is_none());
        let output = String::from_utf8(buf).expect("utf8");
        assert!(output.contains("test-model"));
    }

    #[tokio::test]
    async fn run_core_normal_run() {
        use flick::test_support::SingleShotProvider;

        let config = RequestConfig::parse_yaml("model: test\n").expect("parse");
        let model_info = flick::model_registry::ModelInfo {
            provider: "test".into(),
            name: "test-model".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        };
        let client = FlickClient::new_with_provider(
            config,
            model_info,
            flick::ApiKind::Messages,
            SingleShotProvider::with_text("hello back"),
        );
        let mut ctx = Context::default();
        let mut buf = Vec::new();
        let result = cmd_run_core(&client, &mut ctx, "hello", None, false, &mut buf)
            .await
            .expect("run");
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn run_core_empty_query_rejected() {
        use flick::test_support::SingleShotProvider;

        let config = RequestConfig::parse_yaml("model: test\n").expect("parse");
        let model_info = flick::model_registry::ModelInfo {
            provider: "test".into(),
            name: "test-model".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        };
        let client = FlickClient::new_with_provider(
            config,
            model_info,
            flick::ApiKind::Messages,
            SingleShotProvider::stub(),
        );
        let mut ctx = Context::default();
        let mut buf = Vec::new();
        let result = cmd_run_core(&client, &mut ctx, "", None, false, &mut buf).await;
        assert!(matches!(result, Err(FlickError::NoQuery)));
    }
}
