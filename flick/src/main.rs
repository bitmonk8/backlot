use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use flick::agent;
use flick::config::Config;
use flick::context::Context;
use flick::credential::CredentialStore;
use flick::error::FlickError;
use flick::event::{EventEmitter, JsonLinesEmitter, RawEmitter, Event};
use flick::model::ReasoningLevel;
use flick::model_list::{self, ModelFetcher};
use flick::prompter::{Prompter, TerminalPrompter};
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
    /// List onboarded providers
    List,
    /// Interactive config file generator
    Init {
        /// Output file path (use '-' for stdout)
        #[arg(long, default_value = "flick.toml")]
        output: PathBuf,
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
        Commands::List => (cmd_list().await, false),
        Commands::Init { output } => (cmd_init(output).await, false),
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
    let cred_entry = cred_store.get(cred_name).await?;
    let provider = create_provider(provider_config, cred_entry.key);

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
        use tokio::io::AsyncWriteExt;
        file.write_all(&buf).await.map_err(FlickError::Io)
    }
}

/// Testable core: all dependencies injected.
#[allow(clippy::too_many_lines)]
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

    // Step 5 — Tools
    let tool_items = vec![
        "read_file".to_string(),
        "write_file".to_string(),
        "list_directory".to_string(),
        "shell_exec (unrestricted system access)".to_string(),
    ];
    let tool_defaults = [false, false, false, false];
    let selected_tools = prompter.multi_select("Enable builtin tools", &tool_items, &tool_defaults)?;

    let read_file = selected_tools.contains(&0);
    let write_file = selected_tools.contains(&1);
    let list_directory = selected_tools.contains(&2);
    let mut shell_exec = selected_tools.contains(&3);

    if shell_exec
        && !prompter.confirm(
            "shell_exec grants the model unrestricted system access. Enable?",
            false,
        )?
    {
        shell_exec = false;
    }

    // Step 6 — Write
    let params = ConfigGenParams {
        provider_name,
        model_name: &model_name,
        max_tokens,
        system_prompt: system_prompt.as_deref(),
        api,
        base_url,
        tool_read_file: read_file,
        tool_write_file: write_file,
        tool_list_directory: list_directory,
        tool_shell_exec: shell_exec,
    };
    let toml_output = generate_config_toml(&params);

    if output_path != "-" {
        prompter.message(&format!("Writing config to {output_path}"))?;
    }
    writer.write_all(toml_output.as_bytes()).map_err(FlickError::Io)?;

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
    base_url: &'a str,
    tool_read_file: bool,
    tool_write_file: bool,
    tool_list_directory: bool,
    tool_shell_exec: bool,
}

/// Escape a string for use inside a TOML basic string (`"..."`).
///
/// Escapes backslash, double-quote, and all control characters forbidden by
/// the TOML spec (U+0000–U+001F except U+0009 TAB, plus U+007F DEL).
fn toml_escape_basic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push('\t'),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\u{0000}'..='\u{001F}' | '\u{007F}' => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            _ => out.push(c),
        }
    }
    out
}

fn generate_config_toml(p: &ConfigGenParams<'_>) -> String {
    let mut out = String::new();

    // Header
    out.push_str("# Flick configuration\n");
    out.push_str("# Generated by `flick init`. Edit freely.\n");
    out.push_str("# Reference: docs/CONFIGURATION.md\n");
    out.push('\n');

    // System prompt — top-level key, must appear before any table header
    out.push_str("# ── System Prompt (optional) ────────────────────────────────────────\n");
    match p.system_prompt {
        Some(sp) if (sp.contains('\n') || sp.contains('"')) && !sp.contains("'''") => {
            let _ = writeln!(out, "system_prompt = '''\n{sp}'''");
        }
        Some(sp) => {
            let _ = writeln!(out, "system_prompt = \"{}\"", toml_escape_basic(sp));
        }
        None => {
            out.push_str("# system_prompt = \"You are Flick, a fast LLM runner.\"\n");
        }
    }
    out.push('\n');

    // Model section
    out.push_str("# ── Model ────────────────────────────────────────────────────────────\n");
    out.push_str("# provider: must match a [provider.*] section below\n");
    out.push_str("# name:     model identifier\n");
    out.push_str("# max_tokens: maximum output tokens (omit to use model default, Chat\n");
    out.push_str("#   Completions only; Messages API requires a value)\n");
    out.push_str("# temperature: sampling temperature (optional)\n");
    out.push_str("#   Messages API: 0.0–1.0 | Chat Completions: 0.0–2.0\n");
    out.push_str("[model]\n");
    let _ = writeln!(out, "provider = \"{}\"", toml_escape_basic(p.provider_name));
    let _ = writeln!(out, "name = \"{}\"", toml_escape_basic(p.model_name));
    match p.max_tokens {
        Some(v) => { let _ = writeln!(out, "max_tokens = {v}"); }
        None => out.push_str("# max_tokens = 8192\n"),
    }
    out.push_str("# temperature = 0.0\n");
    out.push('\n');

    // Reasoning section
    out.push_str("# ── Reasoning (optional) ────────────────────────────────────────────\n");
    out.push_str("# level: minimal (1k tokens), low (4k), medium (10k), high (32k)\n");
    out.push_str("# For Messages API: budget must be < max_tokens\n");
    out.push_str("# [model.reasoning]\n");
    out.push_str("# level = \"medium\"\n");
    out.push('\n');

    // Provider section
    out.push_str("# ── Provider ─────────────────────────────────────────────────────────\n");
    out.push_str("# api: \"messages\" (Anthropic) or \"chat_completions\" (OpenAI-compatible)\n");
    out.push_str("# base_url: override the default endpoint (optional)\n");
    out.push_str("# credential: credential store key (defaults to provider name)\n");
    let _ = writeln!(out, "[provider.{}]", p.provider_name);
    let _ = writeln!(out, "api = \"{}\"", p.api);

    let default_base_url = match p.api {
        flick::ApiKind::Messages => "https://api.anthropic.com",
        flick::ApiKind::ChatCompletions => "https://api.openai.com",
    };
    if p.base_url == default_base_url {
        let _ = writeln!(out, "# base_url = \"{}\"", toml_escape_basic(p.base_url));
    } else {
        let _ = writeln!(out, "base_url = \"{}\"", toml_escape_basic(p.base_url));
    }
    let _ = writeln!(out, "# credential = \"{}\"", toml_escape_basic(p.provider_name));
    out.push('\n');

    // Compat flags
    out.push_str("# ── Compatibility Flags (optional) ──────────────────────────────────\n");
    let _ = writeln!(out, "# [provider.{}.compat]", p.provider_name);
    out.push_str("# explicit_tool_choice_auto = false\n");
    out.push('\n');

    // Tools section
    out.push_str("# ── Builtin Tools ───────────────────────────────────────────────────\n");
    out.push_str("# shell_exec and custom tools bypass resource restrictions.\n");
    out.push_str("[tools]\n");
    let _ = writeln!(out, "read_file = {}", p.tool_read_file);
    let _ = writeln!(out, "write_file = {}", p.tool_write_file);
    let _ = writeln!(out, "list_directory = {}", p.tool_list_directory);
    let _ = writeln!(out, "shell_exec = {}", p.tool_shell_exec);
    out.push('\n');

    write_commented_sections(&mut out);

    out
}

fn write_commented_sections(out: &mut String) {
    // Custom tools
    out.push_str("# ── Custom Tools (optional) ─────────────────────────────────────────\n");
    out.push_str("# [[tools.custom]]\n");
    out.push_str("# name = \"my_tool\"\n");
    out.push_str("# description = \"What the tool does\"\n");
    out.push_str("# parameters = { type = \"object\", properties = { arg = { type = \"string\" } } }\n");
    out.push_str("# command = \"echo {{arg}}\"       # shell command (OR executable, not both)\n");
    out.push_str("# executable = \"./tools/my_tool\" # receives JSON on stdin\n");
    out.push('\n');

    // Resources
    out.push_str("# ── Resources (optional) ────────────────────────────────────────────\n");
    out.push_str("# Restricts builtin tool access. If omitted, all paths allowed.\n");
    out.push_str("# Does NOT restrict shell_exec or custom tools.\n");
    out.push_str("# [[resources]]\n");
    out.push_str("# path = \"src/\"\n");
    out.push_str("# access = \"read_write\"\n");
    out.push('\n');

    // Pricing
    out.push_str("# ── Pricing (optional) ──────────────────────────────────────────────\n");
    out.push_str("# Overrides builtin model pricing. Omit to use registry defaults.\n");
    out.push_str("# [pricing]\n");
    out.push_str("# input_per_million = 3.0\n");
    out.push_str("# output_per_million = 15.0\n");
    out.push('\n');

    // Sandbox
    out.push_str("# ── Sandbox (optional) ──────────────────────────────────────────────\n");
    out.push_str("# Wrapper prefix for sandboxed tool execution. See docs/SANDBOX.md.\n");
    out.push_str("# [sandbox]\n");
    out.push_str("# wrapper = [\"bwrap\", \"--die-with-parent\"]\n");
    out.push_str("# read_args = [\"--ro-bind\", \"{path}\", \"{path}\"]\n");
    out.push_str("# read_write_args = [\"--bind\", \"{path}\", \"{path}\"]\n");
    out.push_str("# suffix = [\"--\"]\n");
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
    use flick::prompter::MockPrompter;

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
            context_length: Some(200_000),
        }]);

        // Prompts in order:
        // select provider (0), select model (0), input max_tokens ("64000"),
        // input system_prompt (default), multi_select tools ([]),
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("provider = \"anthropic\""));
        assert!(text.contains("name = \"claude-sonnet-4-20250514\""));
        assert!(text.contains("max_tokens = 64000"));
        assert!(text.contains("api = \"messages\""));
        assert!(text.contains("read_file = false"));
        assert!(text.contains("write_file = false"));
        assert!(text.contains("list_directory = false"));
        assert!(text.contains("shell_exec = false"));
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
            context_length: Some(128_000),
        }]);

        // Select second provider (index 1 = openai), select model (0),
        // input max_tokens, input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![1, 0])
            .with_inputs(vec!["16384".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("provider = \"openai\""));
        assert!(text.contains("api = \"chat_completions\""));
    }

    #[tokio::test]
    async fn init_core_model_fetch_fails_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_error(
            FlickError::Io(std::io::Error::other("network error")),
        );

        // select provider (0), input model name, input max_tokens, input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0])
            .with_inputs(vec!["custom-model".into(), "8192".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("name = \"custom-model\""));
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
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (1 = "(custom)"), input model name,
        // input max_tokens, input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 1])
            .with_inputs(vec!["my-model".into(), "8192".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("name = \"my-model\""));
    }

    #[tokio::test]
    async fn init_core_empty_custom_model_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
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
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (0), input max_tokens ("32000"),
        // input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["32000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("max_tokens = 32000"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_registry_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        // Model matches builtin registry but no max_completion_tokens from provider
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: None,
            context_length: Some(200_000),
        }]);

        // Registry default for claude-sonnet-4-20250514 is 64000
        // select provider (0), select model (0), input max_tokens ("64000"),
        // input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("max_tokens = 64000"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_hardcoded_fallback() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        // Unknown model, no metadata
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "unknown-model-xyz".into(),
            max_completion_tokens: None,
            context_length: None,
        }]);

        // Hardcoded fallback is 8192
        // select provider (0), select model (0), input max_tokens ("8192"),
        // input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["8192".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("max_tokens = 8192"));
    }

    #[tokio::test]
    async fn init_core_max_tokens_none_chat_completions() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("openai", "sk-key", flick::ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set");

        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "gpt-4o".into(),
            max_completion_tokens: Some(16_384),
            context_length: Some(128_000),
        }]);

        // select provider (0), select model (0), input "none",
        // input system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["none".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("# max_tokens = 8192"));
        assert!(!text.contains("\nmax_tokens ="));
    }

    #[tokio::test]
    async fn init_core_system_prompt_with_quotes() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input system prompt with quotes, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec![
                "64000".into(),
                "You are \"Flick\", a fast runner.".into(),
            ])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("system_prompt = '''\nYou are \"Flick\", a fast runner.'''"));
    }

    #[tokio::test]
    async fn init_core_system_prompt_none() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input "none" for system prompt, multi_select tools
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "none".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("# system_prompt = "));
        assert!(!text.contains("\nsystem_prompt = "));
    }

    #[tokio::test]
    async fn init_core_tool_selection() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input system prompt, multi_select tools [0, 2] = read_file + list_directory
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![0, 2]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("read_file = true"));
        assert!(text.contains("write_file = false"));
        assert!(text.contains("list_directory = true"));
        assert!(text.contains("shell_exec = false"));
    }

    #[tokio::test]
    async fn init_core_shell_exec_confirmed() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input system prompt, multi_select tools [3] = shell_exec, confirm true
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![3]])
            .with_confirms(vec![true]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("shell_exec = true"));
    }

    #[tokio::test]
    async fn init_core_shell_exec_rejected() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        // select provider (0), select model (0), input max_tokens,
        // input system prompt, multi_select tools [3] = shell_exec, confirm false
        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![3]])
            .with_confirms(vec![false]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("shell_exec = false"));
    }

    #[tokio::test]
    async fn init_core_file_exists_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![]);
        let prompter = MockPrompter::new();

        let existing_file = dir.path().join("flick.toml");
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
            context_length: Some(200_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

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
        assert!(text.contains("[model]"));
    }

    #[tokio::test]
    async fn init_core_nondefault_base_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("litellm", "sk-key", flick::ApiKind::Messages, "http://custom:4000").await.expect("set");

        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("base_url = \"http://custom:4000\""));
        // Should NOT be commented
        assert!(!text.contains("# base_url = \"http://custom:4000\""));
    }

    #[test]
    fn generate_config_toml_round_trip_messages() {
        let params = ConfigGenParams {
            provider_name: "anthropic",
            model_name: "claude-sonnet-4-20250514",
            max_tokens: Some(64_000),
            system_prompt: Some("You are Flick, a fast LLM runner."),
            api: flick::ApiKind::Messages,
            base_url: "https://api.anthropic.com",
            tool_read_file: true,
            tool_write_file: false,
            tool_list_directory: true,
            tool_shell_exec: false,
        };
        let toml_str = generate_config_toml(&params);
        let config = Config::parse(&toml_str).expect("generated TOML should parse");
        assert_eq!(config.model().provider(), "anthropic");
        assert_eq!(config.model().name(), "claude-sonnet-4-20250514");
        assert_eq!(config.model().max_tokens(), Some(64_000));
        assert_eq!(config.system_prompt(), Some("You are Flick, a fast LLM runner."));
        assert!(config.tools().read_file);
        assert!(!config.tools().write_file);
        assert!(config.tools().list_directory);
        assert!(!config.tools().shell_exec);
    }

    #[tokio::test]
    async fn init_core_default_base_url_commented() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec!["64000".into(), "You are Flick, a fast LLM runner.".into()])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("# base_url = \"https://api.anthropic.com\""));
    }

    // -- Test gap #11: system prompt with newlines --

    #[tokio::test]
    async fn init_core_system_prompt_with_newlines() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec![
                "64000".into(),
                "Line one\nLine two\nLine three".into(),
            ])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        // Multi-line prompt should use ''' literal string
        assert!(text.contains("system_prompt = '''\nLine one\nLine two\nLine three'''"));
        // Verify it round-trips through TOML parsing
        let config = Config::parse(&text).expect("generated TOML should parse");
        assert_eq!(config.system_prompt(), Some("Line one\nLine two\nLine three"));
    }

    // -- Test gap #12: system prompt containing ''' --

    #[tokio::test]
    async fn init_core_system_prompt_with_triple_quotes() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = init_store_with_anthropic(dir.path()).await;
        let fetcher = MockModelFetcher::with_models(vec![FetchedModel {
            id: "claude-sonnet-4-20250514".into(),
            max_completion_tokens: Some(64_000),
            context_length: Some(200_000),
        }]);

        let prompter = MockPrompter::new()
            .with_selects(vec![0, 0])
            .with_inputs(vec![
                "64000".into(),
                "Use '''triple quotes''' carefully".into(),
            ])
            .with_multi_selects(vec![vec![]]);

        let mut output = Vec::new();
        cmd_init_core("-", &prompter, &store, &fetcher, &mut output)
            .await
            .expect("init_core");

        let text = String::from_utf8(output).expect("utf8");
        // Contains ''', so must fall through to escaped basic string
        assert!(text.contains("system_prompt = \"Use \\'\\'\\'triple quotes\\'\\'\\' carefully\"")
            || text.contains("system_prompt = \"Use '''triple quotes''' carefully\""));
        // Verify it round-trips through TOML parsing
        let config = Config::parse(&text).expect("generated TOML should parse");
        assert_eq!(config.system_prompt(), Some("Use '''triple quotes''' carefully"));
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
    fn generate_config_toml_round_trip_chat_completions_no_max_tokens() {
        let params = ConfigGenParams {
            provider_name: "openai",
            model_name: "gpt-4o",
            max_tokens: None,
            system_prompt: Some("You are Flick, a fast LLM runner."),
            api: flick::ApiKind::ChatCompletions,
            base_url: "https://api.openai.com",
            tool_read_file: false,
            tool_write_file: false,
            tool_list_directory: false,
            tool_shell_exec: false,
        };
        let toml_str = generate_config_toml(&params);
        // Verify the commented-out max_tokens line is present
        assert!(toml_str.contains("# max_tokens ="), "expected commented-out max_tokens line");
        // Verify no active max_tokens line
        assert!(!toml_str.lines().any(|l| {
            let trimmed = l.trim();
            trimmed.starts_with("max_tokens") && !trimmed.starts_with('#')
        }), "max_tokens should only appear as a comment");
        // Verify the generated TOML parses successfully
        let config = Config::parse(&toml_str).expect("generated TOML with commented max_tokens should parse");
        assert_eq!(config.model().provider(), "openai");
        assert_eq!(config.model().name(), "gpt-4o");
        assert!(config.model().max_tokens().is_none());
    }
}
