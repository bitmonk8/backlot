// FlickAgent: AgentService implementation backed by the Flick library.

use crate::agent::config_gen::{
    self, AssessmentWire, CheckpointWire, DecompositionWire, RecoveryPlanWire, RecoveryWire,
    TaskOutcomeWire, VerificationWire,
};
use crate::agent::nu_session::NuSession;
use crate::agent::prompts;
use crate::agent::tools::{self, AgentMethod, ToolExecResult, ToolGrant};
use crate::agent::{AgentService, TaskContext};
use crate::config::project::{ModelConfig, VerificationStep};
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{LeafResult, Model, RecoveryPlan};
use anyhow::{Context, bail};
use flick::result::ResultStatus;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

const MAX_TOOL_ROUNDS: u32 = 50;

// ---------------------------------------------------------------------------
// Injection seams for testability
// ---------------------------------------------------------------------------

type ClientFactoryFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = anyhow::Result<flick::FlickClient>> + Send + 'a>>;

pub trait ClientFactory: Send + Sync {
    fn build(&self, config: flick::RequestConfig) -> ClientFactoryFuture<'_>;
}

pub trait ToolExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        tool_use_id: String,
        name: &'a str,
        input: &'a JsonValue,
        project_root: &'a Path,
        grant: ToolGrant,
        nu_session: &'a NuSession,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolExecResult> + Send + 'a>>;
}

struct DefaultClientFactory {
    model_registry: flick::ModelRegistry,
    provider_registry: flick::ProviderRegistry,
}

impl ClientFactory for DefaultClientFactory {
    fn build(&self, config: flick::RequestConfig) -> ClientFactoryFuture<'_> {
        Box::pin(async move {
            flick::FlickClient::new(config, &self.model_registry, &self.provider_registry)
                .await
                .map_err(|e| anyhow::anyhow!("failed to create flick client: {e}"))
        })
    }
}

struct DefaultToolExecutor;

impl ToolExecutor for DefaultToolExecutor {
    fn execute<'a>(
        &'a self,
        tool_use_id: String,
        name: &'a str,
        input: &'a JsonValue,
        project_root: &'a Path,
        grant: ToolGrant,
        nu_session: &'a NuSession,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolExecResult> + Send + 'a>> {
        Box::pin(async move {
            tools::execute_tool(tool_use_id, name, input, project_root, grant, nu_session).await
        })
    }
}

/// Build a `ModelRegistry` from epic's `ModelConfig` and credential name.
///
/// Registers three entries ("fast", "balanced", "strong") mapping to the
/// actual model names from `ModelConfig`. The provider field is set to
/// `credential_name` so Flick's `ProviderRegistry` can resolve the API key.
fn build_model_registry(
    model_config: &ModelConfig,
    credential_name: &str,
) -> anyhow::Result<flick::ModelRegistry> {
    let mut map = BTreeMap::new();
    for tier in [Model::Haiku, Model::Sonnet, Model::Opus] {
        map.insert(
            config_gen::model_key(tier).to_string(),
            flick::ModelInfo {
                provider: credential_name.to_string(),
                name: model_config.name_for(tier).to_string(),
                max_tokens: Some(config_gen::default_max_tokens(tier)),
                input_per_million: None,
                output_per_million: None,
            },
        );
    }
    flick::ModelRegistry::from_map(map)
        .map_err(|e| anyhow::anyhow!("failed to build model registry: {e}"))
}

/// `FlickAgent` invokes the Flick library for each agent call.
pub struct FlickAgent {
    project_root: PathBuf,
    call_timeout: Duration,
    verification_steps: Vec<VerificationStep>,
    client_factory: Box<dyn ClientFactory>,
    tool_executor: Box<dyn ToolExecutor>,
    /// When true, skip the eager `NuSession::spawn()` in `run_with_tools`.
    /// Used in tests where the mock `ToolExecutor` never touches the nu session.
    skip_nu_spawn: bool,
}

impl FlickAgent {
    pub fn new(
        project_root: PathBuf,
        credential_name: &str,
        call_timeout: Duration,
        model_config: &ModelConfig,
        verification_steps: Vec<VerificationStep>,
    ) -> anyhow::Result<Self> {
        let model_registry = build_model_registry(model_config, credential_name)?;
        let provider_registry = flick::ProviderRegistry::load_default()
            .map_err(|e| anyhow::anyhow!("failed to load provider registry: {e}"))?;
        Ok(Self {
            project_root,
            call_timeout,
            verification_steps,
            client_factory: Box::new(DefaultClientFactory {
                model_registry,
                provider_registry,
            }),
            tool_executor: Box::new(DefaultToolExecutor),
            skip_nu_spawn: false,
        })
    }

    #[cfg(test)]
    fn with_injected(
        project_root: PathBuf,
        call_timeout: Duration,
        client_factory: Box<dyn ClientFactory>,
        tool_executor: Box<dyn ToolExecutor>,
    ) -> Self {
        Self {
            project_root,
            call_timeout,
            verification_steps: Vec::new(),
            client_factory,
            tool_executor,
            skip_nu_spawn: true,
        }
    }

    // -----------------------------------------------------------------------
    // Core: build a FlickClient from a RequestConfig
    // -----------------------------------------------------------------------

    async fn build_client(
        &self,
        config: flick::RequestConfig,
    ) -> anyhow::Result<flick::FlickClient> {
        self.client_factory.build(config).await
    }

    // -----------------------------------------------------------------------
    // run_structured: single call, no tools, parse structured output
    // -----------------------------------------------------------------------

    async fn run_structured<T: DeserializeOwned>(
        &self,
        config: flick::RequestConfig,
        query: &str,
    ) -> anyhow::Result<T> {
        let client = self.build_client(config).await?;
        let mut context = flick::Context::default();

        let result = tokio::time::timeout(self.call_timeout, client.run(query, &mut context))
            .await
            .map_err(|_| anyhow::anyhow!("flick call timed out after {:?}", self.call_timeout))?
            .map_err(|e| anyhow::anyhow!("flick call failed: {e}"))?;

        check_error(&result)?;
        log_usage(&result);

        // No tools are configured; a ToolCallsPending status means the model
        // hallucinated tool use and the response likely isn't valid JSON.
        if matches!(result.status, ResultStatus::ToolCallsPending) {
            bail!("model requested tool calls in structured-only (no-tool) context");
        }

        let text = extract_text(&result)?;
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse structured output: {text}"))
    }

    // -----------------------------------------------------------------------
    // run_with_tools: tool loop until complete
    // -----------------------------------------------------------------------

    async fn run_with_tools<T: DeserializeOwned>(
        &self,
        config: flick::RequestConfig,
        query: &str,
        grant: ToolGrant,
    ) -> anyhow::Result<T> {
        let client = self.build_client(config).await?;
        let mut context = flick::Context::default();

        // One NuSession per agent call — spawned eagerly, killed when this method returns.
        let nu_session = NuSession::new();
        if !self.skip_nu_spawn {
            nu_session
                .spawn(&self.project_root, grant)
                .await
                .map_err(|e| anyhow::anyhow!("failed to spawn nu session: {e}"))?;
        }

        // Initial call (not counted toward MAX_TOOL_ROUNDS; the limit applies to resume rounds).
        let mut result = tokio::time::timeout(self.call_timeout, client.run(query, &mut context))
            .await
            .map_err(|_| anyhow::anyhow!("flick call timed out after {:?}", self.call_timeout))?
            .map_err(|e| anyhow::anyhow!("flick call failed: {e}"))?;

        for _round in 1..=MAX_TOOL_ROUNDS {
            if !matches!(result.status, ResultStatus::ToolCallsPending) {
                break;
            }

            let tool_calls = extract_tool_calls(&result)?;
            let mut tool_results = Vec::with_capacity(tool_calls.len());
            for (id, name, input) in &tool_calls {
                let r = self
                    .tool_executor
                    .execute(
                        id.clone(),
                        name,
                        input,
                        &self.project_root,
                        grant,
                        &nu_session,
                    )
                    .await;
                tool_results.push(flick::ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: r.content,
                    is_error: r.is_error,
                });
            }

            result =
                tokio::time::timeout(self.call_timeout, client.resume(&mut context, tool_results))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!("flick call timed out after {:?}", self.call_timeout)
                    })?
                    .map_err(|e| anyhow::anyhow!("flick resume failed: {e}"))?;
        }

        if matches!(result.status, ResultStatus::ToolCallsPending) {
            nu_session.kill().await;
            bail!("flick tool loop exceeded {MAX_TOOL_ROUNDS} rounds");
        }

        // Clean up the nu session before returning.
        nu_session.kill().await;

        check_error(&result)?;
        log_usage(&result);

        let text = extract_text(&result)?;
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse structured output: {text}"))
    }

    async fn run_leaf_task(
        &self,
        pair: prompts::PromptPair,
        model: Model,
    ) -> anyhow::Result<LeafResult> {
        let grant = tools::phase_tools(AgentMethod::Execute);
        let config = config_gen::build_execute_leaf_config(&pair.system_prompt, model, grant)?;

        let wire: TaskOutcomeWire = self.run_with_tools(config, &pair.query, grant).await?;
        LeafResult::try_from(wire)
    }

    async fn decompose_with_prompt(
        &self,
        pair: &prompts::PromptPair,
        model: Model,
    ) -> anyhow::Result<DecompositionResult> {
        let grant = tools::phase_tools(AgentMethod::Decompose);
        let config = config_gen::build_decompose_config(&pair.system_prompt, model, grant)?;

        let wire: DecompositionWire = self.run_with_tools(config, &pair.query, grant).await?;
        DecompositionResult::try_from(wire)
    }
}

// ---------------------------------------------------------------------------
// Init exploration (not part of AgentService — standalone call)
// ---------------------------------------------------------------------------

impl FlickAgent {
    /// Run the init exploration agent to detect project build/test/lint setup.
    pub async fn explore_for_init(&self) -> anyhow::Result<config_gen::InitFindingsWire> {
        let system_prompt = "\
You are a project analyzer. Explore the project directory to detect its build system, \
test framework, linters, and formatters.

Look for:
- Build system markers: Cargo.toml, package.json, pyproject.toml, Makefile, CMakeLists.txt, \
build.gradle, go.mod, etc.
- Test frameworks: test directories, test config files (jest.config, pytest.ini, etc.)
- Linters/formatters: clippy, eslint, ruff, black, prettier, golangci-lint, etc.
- CI config: .github/workflows/, .gitlab-ci.yml — extract build/test/lint commands as hints.

Use tools to explore the project directory. Read key config files to understand the setup.
Do NOT look in .git, node_modules, target, or other build artifact directories.

Respond with the required JSON schema.";

        let query = "Explore this project and detect its verification steps \
(build, lint, test, format commands). Read relevant config files to determine the correct commands.";

        let grant = tools::phase_tools(tools::AgentMethod::Analyze);
        let config = config_gen::build_init_config(system_prompt, grant)?;

        self.run_with_tools(config, query, grant).await
    }
}

// ---------------------------------------------------------------------------
// AgentService implementation
// ---------------------------------------------------------------------------

impl AgentService for FlickAgent {
    async fn assess(&self, ctx: &TaskContext) -> anyhow::Result<AssessmentResult> {
        let pair = prompts::build_assess(ctx);
        let config = config_gen::build_assess_config(&pair.system_prompt, Model::Haiku)?;

        let wire: AssessmentWire = self.run_structured(config, &pair.query).await?;
        AssessmentResult::try_from(wire)
    }

    async fn execute_leaf(&self, ctx: &TaskContext, model: Model) -> anyhow::Result<LeafResult> {
        let pair = prompts::build_execute_leaf(ctx);
        self.run_leaf_task(pair, model).await
    }

    async fn fix_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
        failure_reason: &str,
        attempt: u32,
    ) -> anyhow::Result<LeafResult> {
        let pair = prompts::build_fix_leaf(ctx, failure_reason, attempt);
        self.run_leaf_task(pair, model).await
    }

    async fn design_and_decompose(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<DecompositionResult> {
        let pair = prompts::build_design_and_decompose(ctx);
        self.decompose_with_prompt(&pair, model).await
    }

    async fn design_fix_subtasks(
        &self,
        ctx: &TaskContext,
        model: Model,
        verification_issues: &str,
        round: u32,
    ) -> anyhow::Result<DecompositionResult> {
        let pair = prompts::build_design_fix_subtasks(ctx, verification_issues, round);
        self.decompose_with_prompt(&pair, model).await
    }

    async fn verify(&self, ctx: &TaskContext, model: Model) -> anyhow::Result<VerificationResult> {
        let pair = prompts::build_verify(ctx, &self.verification_steps);
        let grant = tools::phase_tools(AgentMethod::Analyze);
        let config = config_gen::build_verify_config(&pair.system_prompt, model, grant)?;

        let wire: VerificationWire = self.run_with_tools(config, &pair.query, grant).await?;
        VerificationResult::try_from(wire)
    }

    async fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> anyhow::Result<CheckpointDecision> {
        let pair = prompts::build_checkpoint(ctx, discoveries);
        let config = config_gen::build_checkpoint_config(&pair.system_prompt, Model::Haiku)?;

        let wire: CheckpointWire = self.run_structured(config, &pair.query).await?;
        CheckpointDecision::try_from(wire)
    }

    async fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> anyhow::Result<Option<String>> {
        let pair = prompts::build_assess_recovery(ctx, failure_reason);
        let config = config_gen::build_recovery_config(&pair.system_prompt, Model::Opus)?;

        let wire: RecoveryWire = self.run_structured(config, &pair.query).await?;
        Ok(wire.into_strategy())
    }

    async fn design_recovery_subtasks(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
        strategy: &str,
        recovery_round: u32,
    ) -> anyhow::Result<RecoveryPlan> {
        let pair =
            prompts::build_design_recovery_subtasks(ctx, failure_reason, strategy, recovery_round);
        let grant = tools::phase_tools(AgentMethod::Decompose);
        let config = config_gen::build_recovery_plan_config(&pair.system_prompt, Model::Opus, grant)?;

        let wire: RecoveryPlanWire = self.run_with_tools(config, &pair.query, grant).await?;
        RecoveryPlan::try_from(wire)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn log_usage(result: &flick::FlickResult) {
    if let Some(u) = &result.usage {
        eprintln!(
            "[flick] tokens in={} out={} cost=${:.4}",
            u.input_tokens, u.output_tokens, u.cost_usd
        );
    }
}

fn check_error(result: &flick::FlickResult) -> anyhow::Result<()> {
    if matches!(result.status, ResultStatus::Error) {
        let msg = result
            .error
            .as_ref()
            .map_or("unknown flick error", |e| &e.message);
        bail!("flick returned error: {msg}");
    }
    Ok(())
}

fn extract_text(result: &flick::FlickResult) -> anyhow::Result<String> {
    let mut last_text: Option<&str> = None;
    for block in &result.content {
        if let flick::ContentBlock::Text { text } = block {
            last_text = Some(text.as_str());
        }
    }

    last_text
        .map(ToOwned::to_owned)
        .context("no text block found in flick output")
}

fn extract_tool_calls(
    result: &flick::FlickResult,
) -> anyhow::Result<Vec<(String, String, JsonValue)>> {
    let calls: Vec<_> = result
        .content
        .iter()
        .filter_map(|b| match b {
            flick::ContentBlock::ToolUse { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect();

    if calls.is_empty() {
        bail!("tool_calls_pending but no tool_use blocks found");
    }

    Ok(calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_model_registry_produces_correct_entries() {
        let cfg = ModelConfig {
            fast: "my-haiku".into(),
            balanced: "my-sonnet".into(),
            strong: "my-opus".into(),
        };
        let registry = build_model_registry(&cfg, "my-cred").unwrap();

        let fast = registry.get("fast").expect("missing 'fast' entry");
        assert_eq!(fast.name, "my-haiku");
        assert_eq!(fast.provider, "my-cred");
        assert_eq!(fast.max_tokens, Some(config_gen::default_max_tokens(Model::Haiku)));

        let balanced = registry.get("balanced").expect("missing 'balanced' entry");
        assert_eq!(balanced.name, "my-sonnet");
        assert_eq!(balanced.provider, "my-cred");
        assert_eq!(balanced.max_tokens, Some(config_gen::default_max_tokens(Model::Sonnet)));

        let strong = registry.get("strong").expect("missing 'strong' entry");
        assert_eq!(strong.name, "my-opus");
        assert_eq!(strong.provider, "my-cred");
        assert_eq!(strong.max_tokens, Some(config_gen::default_max_tokens(Model::Opus)));
    }

    #[test]
    fn extract_text_from_result() {
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![
                flick::ContentBlock::Thinking {
                    text: "hmm".into(),
                    signature: String::new(),
                },
                flick::ContentBlock::Text {
                    text: "Here is my analysis of the task.".into(),
                },
                flick::ContentBlock::Text {
                    text: r#"{"path":"leaf","model":"haiku","rationale":"simple"}"#.into(),
                },
            ],
            usage: None,
            context_hash: None,
            error: None,
        };
        let text = extract_text(&result).unwrap();
        // Must return the last text block, where structured JSON output lives.
        assert!(text.contains("leaf"));
        assert!(!text.contains("analysis"));
    }

    #[test]
    fn extract_text_missing() {
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![flick::ContentBlock::Thinking {
                text: "hmm".into(),
                signature: String::new(),
            }],
            usage: None,
            context_hash: None,
            error: None,
        };
        assert!(extract_text(&result).is_err());
    }

    #[test]
    fn extract_tool_calls_from_result() {
        let result = flick::FlickResult {
            status: ResultStatus::ToolCallsPending,
            content: vec![
                flick::ContentBlock::Text {
                    text: "let me check".into(),
                },
                flick::ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "Read".into(),
                    input: serde_json::json!({"file_path": "src/main.rs"}),
                },
            ],
            usage: None,
            context_hash: Some("abc123".into()),
            error: None,
        };
        let calls = extract_tool_calls(&result).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "tu_1");
        assert_eq!(calls[0].1, "Read");
    }

    #[test]
    fn check_error_on_error_status() {
        let result = flick::FlickResult {
            status: ResultStatus::Error,
            content: vec![],
            usage: None,
            context_hash: None,
            error: Some(flick::result::ResultError {
                message: "rate limited".into(),
                code: "429".into(),
            }),
        };
        let err = check_error(&result).unwrap_err();
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn check_error_on_complete() {
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![],
            usage: None,
            context_hash: None,
            error: None,
        };
        assert!(check_error(&result).is_ok());
    }

    #[test]
    fn check_error_unknown_when_no_error_field() {
        let result = flick::FlickResult {
            status: ResultStatus::Error,
            content: vec![],
            usage: None,
            context_hash: None,
            error: None,
        };
        let err = check_error(&result).unwrap_err();
        assert!(err.to_string().contains("unknown flick error"));
    }

    #[test]
    fn check_error_passes_tool_calls_pending() {
        // check_error only rejects Error status; ToolCallsPending is caught
        // separately by run_structured's own guard.
        let result = flick::FlickResult {
            status: ResultStatus::ToolCallsPending,
            content: vec![],
            usage: None,
            context_hash: None,
            error: None,
        };
        assert!(check_error(&result).is_ok());
    }

    #[test]
    fn extract_tool_calls_empty_bails() {
        let result = flick::FlickResult {
            status: ResultStatus::ToolCallsPending,
            content: vec![flick::ContentBlock::Text {
                text: "thinking...".into(),
            }],
            usage: None,
            context_hash: None,
            error: None,
        };
        let err = extract_tool_calls(&result).unwrap_err();
        assert!(err.to_string().contains("no tool_use blocks"));
    }

    // -----------------------------------------------------------------------
    // Injection seam tests
    // -----------------------------------------------------------------------

    use flick::test_support::{MultiShotProvider, SingleShotProvider};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn test_model_info() -> flick::ModelInfo {
        flick::ModelInfo {
            provider: "test".into(),
            name: "test-model".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
        }
    }

    /// Client factory that wraps any `Fn() -> Box<dyn DynProvider>` factory.
    struct FnClientFactory<F: Fn() -> Box<dyn flick::DynProvider> + Send + Sync>(F);

    impl<F: Fn() -> Box<dyn flick::DynProvider> + Send + Sync> ClientFactory
        for FnClientFactory<F>
    {
        fn build<'a>(&'a self, config: flick::RequestConfig) -> ClientFactoryFuture<'a> {
            let provider = (self.0)();
            Box::pin(async move {
                Ok(flick::FlickClient::new_with_provider(
                    config,
                    test_model_info(),
                    flick::ApiKind::Messages,
                    provider,
                ))
            })
        }
    }

    fn mock_client_factory<F: Fn() -> Box<dyn flick::DynProvider> + Send + Sync + 'static>(
        factory: F,
    ) -> Box<dyn ClientFactory> {
        Box::new(FnClientFactory(factory))
    }

    fn test_agent(
        client_factory: Box<dyn ClientFactory>,
        executor: Box<dyn ToolExecutor>,
    ) -> FlickAgent {
        FlickAgent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(30),
            client_factory,
            executor,
        )
    }

    struct CountingToolExecutor {
        call_count: Arc<AtomicU32>,
    }

    impl ToolExecutor for CountingToolExecutor {
        fn execute<'a>(
            &'a self,
            tool_use_id: String,
            _name: &'a str,
            _input: &'a JsonValue,
            _project_root: &'a Path,
            _grant: ToolGrant,
            _nu_session: &'a NuSession,
        ) -> Pin<Box<dyn std::future::Future<Output = ToolExecResult> + Send + 'a>> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                ToolExecResult {
                    tool_use_id,
                    content: "mock result".into(),
                    is_error: false,
                }
            })
        }
    }

    fn test_config() -> flick::RequestConfig {
        flick::RequestConfig::parse_yaml("model: test\n").expect("test config should parse")
    }

    #[tokio::test]
    async fn build_client_uses_injected_factory() {
        let agent = test_agent(
            mock_client_factory(|| SingleShotProvider::with_text(r#"{"status":"success"}"#)),
            Box::new(DefaultToolExecutor),
        );
        let client = agent.build_client(test_config()).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn run_structured_with_mock_provider() {
        let agent = test_agent(
            mock_client_factory(|| SingleShotProvider::with_text(r#"{"status":"success"}"#)),
            Box::new(DefaultToolExecutor),
        );
        let result: anyhow::Result<serde_json::Value> =
            agent.run_structured(test_config(), "test query").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["status"], "success");
    }

    fn tool_then_complete_factory() -> Box<dyn ClientFactory> {
        mock_client_factory(|| {
            MultiShotProvider::new(vec![
                flick::provider::ModelResponse {
                    text: None,
                    thinking: Vec::new(),
                    tool_calls: vec![flick::provider::ToolCallResponse {
                        call_id: "tc_1".into(),
                        tool_name: "Read".into(),
                        arguments: r#"{"file_path":"/tmp/test"}"#.into(),
                    }],
                    usage: flick::provider::UsageResponse::default(),
                },
                flick::provider::ModelResponse {
                    text: Some(r#"{"done":true}"#.into()),
                    thinking: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: flick::provider::UsageResponse::default(),
                },
            ])
        })
    }

    #[tokio::test]
    async fn run_with_tools_calls_injected_executor() {
        let tool_calls = Arc::new(AtomicU32::new(0));
        let agent = test_agent(
            tool_then_complete_factory(),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls),
            }),
        );
        let result: anyhow::Result<serde_json::Value> = agent
            .run_with_tools(test_config(), "test", ToolGrant::NU)
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["done"], true);
        assert_eq!(tool_calls.load(Ordering::Relaxed), 1);
    }

    // -----------------------------------------------------------------------
    // Issue 12: MAX_TOOL_ROUNDS exceeded
    // -----------------------------------------------------------------------

    /// Provider that always returns a tool call and never completes.
    struct AlwaysToolCallProvider;

    impl flick::DynProvider for AlwaysToolCallProvider {
        fn call_boxed<'a>(
            &'a self,
            _params: flick::provider::RequestParams<'a>,
        ) -> Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            flick::provider::ModelResponse,
                            flick::error::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                Ok(flick::provider::ModelResponse {
                    text: None,
                    thinking: Vec::new(),
                    tool_calls: vec![flick::provider::ToolCallResponse {
                        call_id: "tc_loop".into(),
                        tool_name: "Read".into(),
                        arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                    }],
                    usage: flick::provider::UsageResponse::default(),
                })
            })
        }

        fn build_request(
            &self,
            _params: flick::provider::RequestParams<'_>,
        ) -> Result<serde_json::Value, flick::error::ProviderError> {
            Ok(serde_json::json!({"model": "test"}))
        }
    }

    #[tokio::test]
    async fn run_with_tools_exceeds_max_rounds() {
        let agent = FlickAgent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(60),
            mock_client_factory(|| {
                Box::new(AlwaysToolCallProvider) as Box<dyn flick::DynProvider>
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::new(AtomicU32::new(0)),
            }),
        );
        let result: anyhow::Result<serde_json::Value> = agent
            .run_with_tools(test_config(), "loop forever", ToolGrant::NU)
            .await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("exceeded"),
            "expected 'exceeded' in error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Issue 13: Timeout path
    // -----------------------------------------------------------------------

    struct SlowProvider;

    impl flick::DynProvider for SlowProvider {
        fn call_boxed<'a>(
            &'a self,
            _params: flick::provider::RequestParams<'a>,
        ) -> Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            flick::provider::ModelResponse,
                            flick::error::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(flick::provider::ModelResponse {
                    text: Some("never reached".into()),
                    thinking: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: flick::provider::UsageResponse::default(),
                })
            })
        }

        fn build_request(
            &self,
            _params: flick::provider::RequestParams<'_>,
        ) -> Result<serde_json::Value, flick::error::ProviderError> {
            Ok(serde_json::json!({"model": "test"}))
        }
    }

    #[tokio::test]
    async fn run_structured_times_out() {
        let agent = FlickAgent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_millis(10),
            mock_client_factory(|| Box::new(SlowProvider) as Box<dyn flick::DynProvider>),
            Box::new(DefaultToolExecutor),
        );
        let result: anyhow::Result<serde_json::Value> =
            agent.run_structured(test_config(), "slow query").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected 'timed out' in error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Issue 14: ClientFactory failure
    // -----------------------------------------------------------------------

    struct FailingClientFactory;

    impl ClientFactory for FailingClientFactory {
        fn build<'a>(&'a self, _config: flick::RequestConfig) -> ClientFactoryFuture<'a> {
            Box::pin(async { Err(anyhow::anyhow!("factory broke")) })
        }
    }

    #[tokio::test]
    async fn build_client_propagates_factory_error() {
        let agent = test_agent(Box::new(FailingClientFactory), Box::new(DefaultToolExecutor));
        match agent.build_client(test_config()).await {
            Ok(_) => panic!("expected factory error, got Ok"),
            Err(err) => assert!(
                err.to_string().contains("factory broke"),
                "expected 'factory broke' in error, got: {err}"
            ),
        }
    }
}
