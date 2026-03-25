// Agent: tool loop runtime.
//
// Manages a single agent session: builds request config, runs the tool loop
// (dispatch tool calls to built-in or custom handlers), and returns
// structured output.

use crate::nu_session::NuSession;
use crate::tools::{self, ToolDefinition, ToolExecResult, ToolGrant};
use anyhow::{Context, bail};
use flick::result::ResultStatus;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

const MAX_TOOL_ROUNDS: u32 = 50;
const MAX_TOOL_CALLS: u32 = 200;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Consumer-implemented trait for custom tools beyond the 6 built-ins.
///
/// Each implementation bundles the tool's schema (what the model sees)
/// with its execution logic (what happens when the model calls it).
pub trait ToolHandler: Send + Sync {
    /// Returns the tool definition included in the model's tool list.
    fn definition(&self) -> ToolDefinition;

    /// Executes the tool call. Called by reel's tool loop when the model
    /// invokes a tool whose name matches `definition().name`.
    fn execute<'a>(
        &'a self,
        tool_use_id: String,
        input: &'a JsonValue,
    ) -> Pin<Box<dyn std::future::Future<Output = ToolExecResult> + Send + 'a>>;
}

/// Shared runtime context that doesn't change between agent calls.
pub struct AgentEnvironment {
    pub model_registry: flick::ModelRegistry,
    pub provider_registry: flick::ProviderRegistry,
    pub project_root: PathBuf,
    pub timeout: Duration,
}

/// Per-call configuration for an agent session. Wraps a flick `RequestConfig`
/// with reel-specific fields. Reusable across calls — query is passed separately.
pub struct AgentRequestConfig {
    /// Request config (model, system_prompt, temperature, reasoning,
    /// output_schema). Reel injects built-in tool definitions into this
    /// before passing it to the provider client.
    pub config: flick::RequestConfig,

    /// Tool grant controlling which built-in tools are available.
    pub grant: ToolGrant,

    /// Consumer-provided tools beyond the built-ins.
    pub custom_tools: Vec<Box<dyn ToolHandler>>,

    /// Additional writable subdirectories within the project root.
    ///
    /// When the base grant includes `TOOLS` but not `WRITE` (read-only root),
    /// each path listed here is added as a write-path in the sandbox policy,
    /// allowing the agent to write to specific subdirectories while the rest
    /// of the project root remains read-only.
    ///
    /// Ignored when the base grant includes `WRITE` (entire project root is
    /// already writable). Each entry must be a child of `project_root`;
    /// lot validates this at policy-build time.
    pub write_paths: Vec<PathBuf>,
}

/// Usage statistics from an agent run.
#[derive(Debug, Clone)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// Result of an agent run.
#[derive(Debug)]
pub struct RunResult<T> {
    pub output: T,
    pub usage: Option<Usage>,
    pub tool_calls: u32,
    pub response_hash: Option<String>,
}

// ---------------------------------------------------------------------------
// Injection seams for testability
// ---------------------------------------------------------------------------

type ClientFactoryFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = anyhow::Result<flick::FlickClient>> + Send + 'a>>;

trait ClientFactory: Send + Sync {
    fn build(&self, config: flick::RequestConfig) -> ClientFactoryFuture<'_>;
}

trait ToolExecutor: Send + Sync {
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
                .map_err(|e| anyhow::anyhow!("failed to create client: {e}"))
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

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Agent session runtime. Owns the tool loop and built-in tool execution.
pub struct Agent {
    project_root: PathBuf,
    timeout: Duration,
    client_factory: Box<dyn ClientFactory>,
    tool_executor: Box<dyn ToolExecutor>,
    /// When true, skip eager `NuSession::spawn()` in `run_with_tools`.
    /// Used in tests where the mock `ToolExecutor` never touches the nu session.
    skip_nu_spawn: bool,
}

impl Agent {
    /// Create an agent from a shared environment.
    pub fn new(env: AgentEnvironment) -> Self {
        Self {
            project_root: env.project_root,
            timeout: env.timeout,
            client_factory: Box::new(DefaultClientFactory {
                model_registry: env.model_registry,
                provider_registry: env.provider_registry,
            }),
            tool_executor: Box::new(DefaultToolExecutor),
            skip_nu_spawn: false,
        }
    }

    #[cfg(test)]
    fn with_injected(
        project_root: PathBuf,
        timeout: Duration,
        client_factory: Box<dyn ClientFactory>,
        tool_executor: Box<dyn ToolExecutor>,
    ) -> Self {
        Self {
            project_root,
            timeout,
            client_factory,
            tool_executor,
            skip_nu_spawn: true,
        }
    }

    /// Run an agent session. Dispatches to structured (no tools) or
    /// tool-loop mode based on whether any tools (built-in or custom) are available.
    pub async fn run<T: DeserializeOwned>(
        &self,
        request: &AgentRequestConfig,
        query: &str,
    ) -> anyhow::Result<RunResult<T>> {
        let has_tools =
            !tools::tool_definitions(request.grant).is_empty() || !request.custom_tools.is_empty();
        if has_tools {
            self.run_with_tools(request, query).await
        } else {
            self.run_structured(request, query).await
        }
    }

    // -----------------------------------------------------------------------
    // Config building
    // -----------------------------------------------------------------------

    /// Build the effective request config with tool definitions injected.
    /// Useful for dry-run / debugging, or called internally before model invocation.
    pub fn build_request_config(
        request: &AgentRequestConfig,
    ) -> anyhow::Result<flick::RequestConfig> {
        let built_in_tools = tools::tool_definitions(request.grant);
        let custom_tool_defs = request.custom_tools.iter().map(|h| h.definition());

        let all_tools: Vec<flick::ToolConfig> = built_in_tools
            .into_iter()
            .chain(custom_tool_defs)
            .map(|t| flick::ToolConfig::new(t.name, t.description, Some(t.parameters)))
            .collect();

        // Clone so new flick fields are preserved without manual forwarding.
        let mut config = request.config.clone();

        if !all_tools.is_empty() {
            config
                .add_tools(all_tools)
                .map_err(|e| anyhow::anyhow!("failed to add tools to config: {e}"))?;
        }

        Ok(config)
    }

    // -----------------------------------------------------------------------
    // run_structured: single call, no tools, parse structured output
    // -----------------------------------------------------------------------

    async fn run_structured<T: DeserializeOwned>(
        &self,
        request: &AgentRequestConfig,
        query: &str,
    ) -> anyhow::Result<RunResult<T>> {
        let config = Self::build_request_config(request)?;
        let client = self.client_factory.build(config).await?;
        let mut context = flick::Context::default();

        let result = tokio::time::timeout(self.timeout, client.run(query, &mut context))
            .await
            .map_err(|_| anyhow::anyhow!("agent call timed out after {:?}", self.timeout))?
            .map_err(|e| anyhow::anyhow!("agent call failed: {e}"))?;

        check_error(&result)?;

        if matches!(result.status, ResultStatus::ToolCallsPending) {
            bail!("model requested tool calls in structured-only (no-tool) context");
        }

        finalize_result(&result, 0)
    }

    // -----------------------------------------------------------------------
    // run_with_tools: tool loop until complete
    // -----------------------------------------------------------------------

    async fn run_with_tools<T: DeserializeOwned>(
        &self,
        request: &AgentRequestConfig,
        query: &str,
    ) -> anyhow::Result<RunResult<T>> {
        let config = Self::build_request_config(request)?;
        let client = self.client_factory.build(config).await?;
        let mut context = flick::Context::default();

        let nu_session = NuSession::new();
        if !self.skip_nu_spawn {
            nu_session
                .spawn(&self.project_root, request.grant, &request.write_paths)
                .await
                .map_err(|e| anyhow::anyhow!("failed to spawn nu session: {e}"))?;
        }

        // Build custom tool name→index map once to avoid calling definition() per dispatch.
        let custom_tool_index: HashMap<String, usize> = request
            .custom_tools
            .iter()
            .enumerate()
            .map(|(i, h)| (h.definition().name, i))
            .collect();

        let mut result = tokio::time::timeout(self.timeout, client.run(query, &mut context))
            .await
            .map_err(|_| anyhow::anyhow!("agent call timed out after {:?}", self.timeout))?
            .map_err(|e| anyhow::anyhow!("agent call failed: {e}"))?;

        let mut total_tool_calls: u32 = 0;

        for _round in 1..=MAX_TOOL_ROUNDS {
            if !matches!(result.status, ResultStatus::ToolCallsPending) {
                break;
            }

            let tool_calls = extract_tool_calls(&result)?;
            total_tool_calls += tool_calls.len() as u32;
            if total_tool_calls > MAX_TOOL_CALLS {
                nu_session.kill().await;
                bail!("agent tool loop exceeded {MAX_TOOL_CALLS} tool calls");
            }
            let mut tool_results = Vec::with_capacity(tool_calls.len());

            for (id, name, input) in &tool_calls {
                let r = self
                    .dispatch_tool(
                        id.clone(),
                        name,
                        input,
                        request.grant,
                        &nu_session,
                        &request.custom_tools,
                        &custom_tool_index,
                    )
                    .await;
                tool_results.push(flick::ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: r.content,
                    is_error: r.is_error,
                });
            }

            result = tokio::time::timeout(self.timeout, client.resume(&mut context, tool_results))
                .await
                .map_err(|_| anyhow::anyhow!("agent call timed out after {:?}", self.timeout))?
                .map_err(|e| anyhow::anyhow!("agent resume failed: {e}"))?;
        }

        if matches!(result.status, ResultStatus::ToolCallsPending) {
            nu_session.kill().await;
            bail!("agent tool loop exceeded {MAX_TOOL_ROUNDS} rounds");
        }

        nu_session.kill().await;

        check_error(&result)?;

        finalize_result(&result, total_tool_calls)
    }

    // -----------------------------------------------------------------------
    // Tool dispatch
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_tool(
        &self,
        tool_use_id: String,
        name: &str,
        input: &JsonValue,
        grant: ToolGrant,
        nu_session: &NuSession,
        custom_tools: &[Box<dyn ToolHandler>],
        custom_tool_index: &HashMap<String, usize>,
    ) -> ToolExecResult {
        // Custom tools first — allows consumers to override built-in tools if needed.
        if let Some(&idx) = custom_tool_index.get(name) {
            return custom_tools[idx].execute(tool_use_id, input).await;
        }

        // Built-in tools via nu session.
        self.tool_executor
            .execute(
                tool_use_id,
                name,
                input,
                &self.project_root,
                grant,
                nu_session,
            )
            .await
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn finalize_result<T: DeserializeOwned>(
    result: &flick::FlickResult,
    tool_calls: u32,
) -> anyhow::Result<RunResult<T>> {
    let text = extract_text(result)?;
    // Try JSON parse first. If the text isn't valid JSON (e.g. free-form
    // model output when no output_schema is set), wrap it as a JSON string
    // and try again — this succeeds when T is serde_json::Value or String.
    let output: T = serde_json::from_str(&text).or_else(|orig_err| {
        let quoted = serde_json::to_string(&text).with_context(|| "string serialization failed")?;
        serde_json::from_str(&quoted)
            .with_context(|| format!("failed to parse model output ({orig_err}): {text}"))
    })?;

    let usage = result.usage.as_ref().map(|u| Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cost_usd: u.cost_usd,
    });

    Ok(RunResult {
        output,
        usage,
        tool_calls,
        response_hash: result.context_hash.clone(),
    })
}

fn check_error(result: &flick::FlickResult) -> anyhow::Result<()> {
    if matches!(result.status, ResultStatus::Error) {
        let msg = result
            .error
            .as_ref()
            .map_or("unknown error", |e| &e.message);
        bail!("agent returned error: {msg}");
    }
    Ok(())
}

fn extract_text(result: &flick::FlickResult) -> anyhow::Result<String> {
    result
        .content
        .iter()
        .rev()
        .find_map(|block| match block {
            flick::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .context("no text block found in model output")
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
        let text = extract_text(&result).unwrap_or_default();
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
        let calls = extract_tool_calls(&result).unwrap_or_default();
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
        assert!(
            err.to_string().contains("rate limited"),
            "expected 'rate limited' in error, got: {err}"
        );
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
        assert!(
            err.to_string().contains("unknown error"),
            "expected 'unknown error' in error, got: {err}"
        );
    }

    #[test]
    fn check_error_passes_tool_calls_pending() {
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
        assert!(extract_tool_calls(&result).is_err());
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
            cache_creation_per_million: None,
            cache_read_per_million: None,
        }
    }

    fn text_response(text: &str) -> flick::provider::ModelResponse {
        flick::provider::ModelResponse {
            text: Some(text.into()),
            thinking: Vec::new(),
            tool_calls: Vec::new(),
            usage: flick::provider::UsageResponse::default(),
        }
    }

    fn tool_call_response(
        calls: Vec<flick::provider::ToolCallResponse>,
    ) -> flick::provider::ModelResponse {
        flick::provider::ModelResponse {
            text: None,
            thinking: Vec::new(),
            tool_calls: calls,
            usage: flick::provider::UsageResponse::default(),
        }
    }

    /// Client factory that wraps any `Fn() -> Box<dyn DynProvider>` factory.
    struct FnClientFactory<F: Fn() -> Box<dyn flick::DynProvider> + Send + Sync>(F);

    impl<F: Fn() -> Box<dyn flick::DynProvider> + Send + Sync> ClientFactory for FnClientFactory<F> {
        fn build(&self, config: flick::RequestConfig) -> ClientFactoryFuture<'_> {
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
    ) -> Agent {
        Agent::with_injected(
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

    fn test_request() -> AgentRequestConfig {
        AgentRequestConfig {
            config: flick::RequestConfig::builder()
                .model("test")
                .build()
                .expect("test config"),
            grant: ToolGrant::empty(),
            custom_tools: Vec::new(),
            write_paths: Vec::new(),
        }
    }

    #[tokio::test]
    async fn run_structured_with_mock_provider() {
        let agent = test_agent(
            mock_client_factory(|| SingleShotProvider::with_text(r#"{"status":"success"}"#)),
            Box::new(DefaultToolExecutor),
        );
        let request = test_request();
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap_or_else(|e| panic!("{e}")).output["status"],
            "success"
        );
    }

    fn tool_then_complete_factory() -> Box<dyn ClientFactory> {
        mock_client_factory(|| {
            MultiShotProvider::new(vec![
                tool_call_response(vec![flick::provider::ToolCallResponse {
                    call_id: "tc_1".into(),
                    tool_name: "Read".into(),
                    arguments: r#"{"file_path":"/tmp/test"}"#.into(),
                }]),
                text_response(r#"{"done":true}"#),
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
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(result.is_ok());
        let r = result.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.output["done"], true);
        assert_eq!(tool_calls.load(Ordering::Relaxed), 1);
        assert_eq!(r.tool_calls, 1);
    }

    // -----------------------------------------------------------------------
    // MAX_TOOL_ROUNDS / MAX_TOOL_CALLS exceeded
    // -----------------------------------------------------------------------

    /// Provider that always returns N tool calls per round, never completing.
    struct RepeatingToolCallProvider {
        calls_per_round: usize,
    }

    impl flick::DynProvider for RepeatingToolCallProvider {
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
            let calls: Vec<_> = (0..self.calls_per_round)
                .map(|i| flick::provider::ToolCallResponse {
                    call_id: format!("tc_{i}"),
                    tool_name: "Read".into(),
                    arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                })
                .collect();
            Box::pin(async move {
                Ok(flick::provider::ModelResponse {
                    text: None,
                    thinking: Vec::new(),
                    tool_calls: calls,
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
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(60),
            mock_client_factory(|| {
                Box::new(RepeatingToolCallProvider { calls_per_round: 1 })
                    as Box<dyn flick::DynProvider>
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::new(AtomicU32::new(0)),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("rounds"),
            "expected 'rounds' in error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Timeout path
    // -----------------------------------------------------------------------

    /// Provider where the first `fast_calls` invocations return a tool call
    /// immediately, then all subsequent calls sleep for 60 s (triggering timeout).
    /// Use `fast_calls: 0` for always-slow behavior.
    struct DelayProvider {
        call_count: AtomicU32,
        fast_calls: u32,
    }

    impl flick::DynProvider for DelayProvider {
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
            let call = self.call_count.fetch_add(1, Ordering::Relaxed);
            if call < self.fast_calls {
                Box::pin(async {
                    Ok(tool_call_response(vec![
                        flick::provider::ToolCallResponse {
                            call_id: "tc_1".into(),
                            tool_name: "Read".into(),
                            arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                        },
                    ]))
                })
            } else {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok(text_response("never reached"))
                })
            }
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
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_millis(10),
            mock_client_factory(|| {
                Box::new(DelayProvider {
                    call_count: AtomicU32::new(0),
                    fast_calls: 0,
                }) as Box<dyn flick::DynProvider>
            }),
            Box::new(DefaultToolExecutor),
        );
        let request = test_request();
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected 'timed out' in error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // ClientFactory failure
    // -----------------------------------------------------------------------

    struct FailingClientFactory;

    impl ClientFactory for FailingClientFactory {
        fn build(&self, _config: flick::RequestConfig) -> ClientFactoryFuture<'_> {
            Box::pin(async { Err(anyhow::anyhow!("factory broke")) })
        }
    }

    // -----------------------------------------------------------------------
    // ToolHandler dispatch tests (issue #1)
    // -----------------------------------------------------------------------

    /// A mock ToolHandler that records whether it was called.
    struct MockToolHandler {
        tool_name: String,
        response: String,
        call_count: Arc<AtomicU32>,
    }

    impl MockToolHandler {
        fn new(name: &str, response: &str) -> Self {
            Self {
                tool_name: name.into(),
                response: response.into(),
                call_count: Arc::new(AtomicU32::new(0)),
            }
        }
    }

    impl ToolHandler for MockToolHandler {
        fn definition(&self) -> tools::ToolDefinition {
            tools::ToolDefinition {
                name: self.tool_name.clone(),
                description: "mock tool".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            }
        }

        fn execute<'a>(
            &'a self,
            tool_use_id: String,
            _input: &'a JsonValue,
        ) -> Pin<Box<dyn std::future::Future<Output = ToolExecResult> + Send + 'a>> {
            self.call_count.fetch_add(1, Ordering::Relaxed);
            let response = self.response.clone();
            Box::pin(async move {
                ToolExecResult {
                    tool_use_id,
                    content: response,
                    is_error: false,
                }
            })
        }
    }

    #[tokio::test]
    async fn dispatch_custom_tool_by_name() {
        let handler = MockToolHandler::new("MyCustomTool", "custom result");
        let call_count = Arc::clone(&handler.call_count);

        let factory = mock_client_factory(|| {
            MultiShotProvider::new(vec![
                tool_call_response(vec![flick::provider::ToolCallResponse {
                    call_id: "tc_custom".into(),
                    tool_name: "MyCustomTool".into(),
                    arguments: "{}".into(),
                }]),
                text_response(r#"{"result":"ok"}"#),
            ])
        });

        // Use a CountingToolExecutor so we can verify the built-in executor was NOT called.
        let builtin_calls = Arc::new(AtomicU32::new(0));
        let agent = test_agent(
            factory,
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&builtin_calls),
            }),
        );

        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        request.custom_tools = vec![Box::new(handler)];

        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
        assert_eq!(builtin_calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn custom_tool_with_builtin_name_rejected_as_duplicate() {
        // A custom tool named "Read" collides with the built-in Read tool.
        // Flick rejects duplicate tool names at config build time.
        let handler = MockToolHandler::new("Read", "custom read override");

        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        request.custom_tools = vec![Box::new(handler)];

        let result = Agent::build_request_config(&request);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("duplicate"),
            "expected duplicate error, got: {err}"
        );
    }

    #[tokio::test]
    async fn unknown_tool_falls_through_to_builtin_executor() {
        // When the model calls a tool name not in custom_tools, it goes to the built-in executor.
        let builtin_calls = Arc::new(AtomicU32::new(0));
        let agent = test_agent(
            tool_then_complete_factory(),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&builtin_calls),
            }),
        );

        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        // No custom tools — "Read" call goes to built-in executor.
        request.custom_tools = Vec::new();

        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(result.is_ok());
        assert_eq!(builtin_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn custom_tool_definitions_included_in_config() {
        let handler = MockToolHandler::new("SpecialTool", "result");
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        request.custom_tools = vec![Box::new(handler)];

        let config = Agent::build_request_config(&request).unwrap();
        let tool_names: Vec<&str> = config.tools().iter().map(flick::ToolConfig::name).collect();
        assert!(tool_names.contains(&"SpecialTool"));
        // Built-in tools should also be present.
        assert!(tool_names.contains(&"Read"));
    }

    #[tokio::test]
    async fn build_client_propagates_factory_error() {
        let agent = test_agent(
            Box::new(FailingClientFactory),
            Box::new(DefaultToolExecutor),
        );
        let request = test_request();
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("factory broke"),
            "expected 'factory broke' in error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // ToolCallsPending in structured mode
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_structured_bails_on_tool_calls_pending() {
        // Provider returns ToolCallsPending with a tool_use block.
        let agent = test_agent(
            mock_client_factory(|| {
                SingleShotProvider::with_tool_calls(vec![flick::provider::ToolCallResponse {
                    call_id: "tc_bad".into(),
                    tool_name: "Read".into(),
                    arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                }])
            }),
            Box::new(DefaultToolExecutor),
        );
        // Empty grant + no custom tools => structured mode.
        let request = test_request();
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("tool calls in structured-only"),
            "expected 'tool calls in structured-only' in error, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Multi-tool-call-per-round counting
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_tools_counts_multi_calls_in_round() {
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = test_agent(
            mock_client_factory(|| {
                MultiShotProvider::new(vec![
                    // First response: 3 tool calls in a single round.
                    tool_call_response(vec![
                        flick::provider::ToolCallResponse {
                            call_id: "tc_a".into(),
                            tool_name: "Read".into(),
                            arguments: r#"{"file_path":"/tmp/a"}"#.into(),
                        },
                        flick::provider::ToolCallResponse {
                            call_id: "tc_b".into(),
                            tool_name: "Read".into(),
                            arguments: r#"{"file_path":"/tmp/b"}"#.into(),
                        },
                        flick::provider::ToolCallResponse {
                            call_id: "tc_c".into(),
                            tool_name: "Read".into(),
                            arguments: r#"{"file_path":"/tmp/c"}"#.into(),
                        },
                    ]),
                    // Second response: completion.
                    text_response(r#"{"done":true}"#),
                ])
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(result.is_ok());
        let r = result.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 3);
        assert_eq!(r.tool_calls, 3);
    }

    // -----------------------------------------------------------------------
    // Custom-tools-only routes to tool loop, not structured mode
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn custom_tools_only_routes_to_tool_loop() {
        let handler = MockToolHandler::new("MyTool", "custom result");
        let call_count = Arc::clone(&handler.call_count);

        let factory = mock_client_factory(|| {
            MultiShotProvider::new(vec![
                tool_call_response(vec![flick::provider::ToolCallResponse {
                    call_id: "tc_custom".into(),
                    tool_name: "MyTool".into(),
                    arguments: "{}".into(),
                }]),
                text_response(r#"{"ok":true}"#),
            ])
        });

        let builtin_calls = Arc::new(AtomicU32::new(0));
        let agent = test_agent(
            factory,
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&builtin_calls),
            }),
        );

        let mut request = test_request();
        // No grant — only custom tools. Should still route to tool loop.
        request.grant = ToolGrant::empty();
        request.custom_tools = vec![Box::new(handler)];

        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(result.is_ok());
        let r = result.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.output["ok"], true);
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
        assert_eq!(builtin_calls.load(Ordering::Relaxed), 0);
        assert_eq!(r.tool_calls, 1);
    }

    // -----------------------------------------------------------------------
    // MAX_TOOL_CALLS cap exceeded
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_tools_exceeds_max_tool_calls() {
        // 50 tool calls per round × 5 rounds = 250 > MAX_TOOL_CALLS (200).
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(60),
            mock_client_factory(|| {
                Box::new(RepeatingToolCallProvider {
                    calls_per_round: 50,
                }) as Box<dyn flick::DynProvider>
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("tool calls"),
            "expected 'tool calls' in error, got: {err}"
        );
        // Should have executed exactly 200 calls (4 rounds × 50) before the 5th round trips the cap (250 > 200).
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 200);
    }

    // -----------------------------------------------------------------------
    // Exactly MAX_TOOL_CALLS boundary (#53)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_tools_exactly_max_tool_calls_succeeds() {
        // 50 calls x 4 rounds = 200 = MAX_TOOL_CALLS. Should succeed (cap is >200, not >=200).
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(60),
            mock_client_factory(|| {
                let mut responses: Vec<flick::provider::ModelResponse> = (1..=4)
                    .map(|round| flick::provider::ModelResponse {
                        text: None,
                        thinking: Vec::new(),
                        tool_calls: (0..50)
                            .map(|i| flick::provider::ToolCallResponse {
                                call_id: format!("tc_r{round}_{i}"),
                                tool_name: "Read".into(),
                                arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                            })
                            .collect(),
                        usage: flick::provider::UsageResponse::default(),
                    })
                    .collect();
                responses.push(flick::provider::ModelResponse {
                    text: Some(r#"{"boundary":"ok"}"#.into()),
                    thinking: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: flick::provider::UsageResponse::default(),
                });
                MultiShotProvider::new(responses)
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        assert!(
            result.is_ok(),
            "exactly 200 tool calls should succeed, got: {}",
            result.unwrap_err()
        );
        let r = result.unwrap();
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 200);
        assert_eq!(r.tool_calls, 200);
        assert_eq!(r.output["boundary"], "ok");
    }

    // -----------------------------------------------------------------------
    // Exactly MAX_TOOL_CALLS + 1 boundary (#73)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_tools_exactly_max_tool_calls_plus_one_fails() {
        // 201 = MAX_TOOL_CALLS + 1. Verifies the check is `>` not `>=`.
        // 67 calls/round: round 1 = 67, round 2 = 134, round 3 = 201 > 200 -> bail.
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(60),
            mock_client_factory(|| {
                Box::new(RepeatingToolCallProvider {
                    calls_per_round: 67,
                }) as Box<dyn flick::DynProvider>
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("tool calls"),
            "expected 'tool calls' in error, got: {err}"
        );
        // Only 2 rounds executed (134 calls) before the 3rd round's 67 tripped the cap.
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 134);
    }

    // -----------------------------------------------------------------------
    // Timeout during resume (tool loop) phase (#6)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_tools_times_out_during_resume() {
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_millis(500),
            mock_client_factory(|| {
                Box::new(DelayProvider {
                    call_count: AtomicU32::new(0),
                    fast_calls: 1,
                }) as Box<dyn flick::DynProvider>
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "expected 'timed out' in error, got: {err}"
        );
        // Tool executor should have been called once (before the slow resume).
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 1);
    }

    // -----------------------------------------------------------------------
    // RunResult field propagation (#13)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_structured_propagates_usage() {
        let agent = test_agent(
            mock_client_factory(|| {
                MultiShotProvider::new(vec![flick::provider::ModelResponse {
                    text: Some(r#"{"field_test":true}"#.into()),
                    thinking: Vec::new(),
                    tool_calls: Vec::new(),
                    usage: flick::provider::UsageResponse {
                        input_tokens: 1000,
                        output_tokens: 500,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                    },
                }])
            }),
            Box::new(DefaultToolExecutor),
        );
        let request = test_request();
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let r = result.unwrap_or_else(|e| panic!("{e}"));

        // Output parsed correctly.
        assert_eq!(r.output["field_test"], true);

        // Usage propagated from provider.
        let usage = r.usage.expect("usage should be Some");
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        // cost_usd is 0.0 since test model has no pricing info.
        assert!(usage.cost_usd.abs() < f64::EPSILON);

        // tool_calls should be 0 in structured mode.
        assert_eq!(r.tool_calls, 0);

        // response_hash is always None from the flick runner (context_hash not set).
        assert!(r.response_hash.is_none());
    }

    #[tokio::test]
    async fn run_with_tools_propagates_usage_fields() {
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = test_agent(
            mock_client_factory(|| {
                MultiShotProvider::new(vec![
                    // First response: tool call with non-default usage.
                    flick::provider::ModelResponse {
                        text: None,
                        thinking: Vec::new(),
                        tool_calls: vec![flick::provider::ToolCallResponse {
                            call_id: "tc_1".into(),
                            tool_name: "Read".into(),
                            arguments: r#"{"file_path":"/tmp/test"}"#.into(),
                        }],
                        usage: flick::provider::UsageResponse {
                            input_tokens: 800,
                            output_tokens: 200,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                        },
                    },
                    // Completion with non-default usage.
                    flick::provider::ModelResponse {
                        text: Some(r#"{"usage_test":true}"#.into()),
                        thinking: Vec::new(),
                        tool_calls: Vec::new(),
                        usage: flick::provider::UsageResponse {
                            input_tokens: 1200,
                            output_tokens: 300,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                        },
                    },
                ])
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let r = result.unwrap_or_else(|e| panic!("{e}"));

        // Output correct.
        assert_eq!(r.output["usage_test"], true);

        // Usage from the completing response.
        let usage = r.usage.expect("usage should be Some");
        assert_eq!(usage.input_tokens, 1200);
        assert_eq!(usage.output_tokens, 300);
        // cost_usd is 0.0 since test model has no pricing info.
        assert!(usage.cost_usd.abs() < f64::EPSILON);

        // tool_calls count.
        assert_eq!(r.tool_calls, 1);
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 1);

        // response_hash (always None from runner).
        assert!(r.response_hash.is_none());
    }

    // -----------------------------------------------------------------------
    // Duplicate custom tool name HashMap semantics (#48)
    // -----------------------------------------------------------------------

    #[test]
    fn duplicate_custom_tool_names_last_wins_in_index() {
        // Production builds the index in run_with_tools as:
        //   handlers.iter().enumerate().map(|(i, h)| (h.definition().name, i)).collect()
        // HashMap::collect keeps the last entry for duplicate keys.
        // Note: build_request_config rejects duplicate tool names before this
        // code runs, so this documents defense-in-depth behavior.
        let handlers: Vec<Box<dyn ToolHandler>> = vec![
            Box::new(MockToolHandler::new("Dup", "first")),
            Box::new(MockToolHandler::new("Dup", "second")),
        ];
        // Use the exact same expression as production code.
        let index: HashMap<String, usize> = handlers
            .iter()
            .enumerate()
            .map(|(i, h)| (h.definition().name, i))
            .collect();
        assert_eq!(index["Dup"], 1, "HashMap should keep last entry");
    }

    // -----------------------------------------------------------------------
    // MAX_TOOL_CALLS cap crossed mid-round (#75)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn run_with_tools_cap_crossed_mid_round() {
        // 150 calls in round 1 + 51 in round 2 = 201 > MAX_TOOL_CALLS (200).
        // Unlike the 67-calls/round test, the cap is crossed mid-batch (not at
        // a round boundary), verifying the `>` check inside the loop body.
        let tool_calls_counter = Arc::new(AtomicU32::new(0));
        let agent = Agent::with_injected(
            PathBuf::from("/tmp"),
            Duration::from_secs(60),
            mock_client_factory(|| {
                MultiShotProvider::new(vec![
                    // Round 1: 150 tool calls
                    tool_call_response(
                        (0..150)
                            .map(|i| flick::provider::ToolCallResponse {
                                call_id: format!("tc_r1_{i}"),
                                tool_name: "Read".into(),
                                arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                            })
                            .collect(),
                    ),
                    // Round 2: 51 tool calls — total becomes 201 > 200, bail before executing
                    tool_call_response(
                        (0..51)
                            .map(|i| flick::provider::ToolCallResponse {
                                call_id: format!("tc_r2_{i}"),
                                tool_name: "Read".into(),
                                arguments: r#"{"file_path":"/tmp/x"}"#.into(),
                            })
                            .collect(),
                    ),
                ])
            }),
            Box::new(CountingToolExecutor {
                call_count: Arc::clone(&tool_calls_counter),
            }),
        );
        let mut request = test_request();
        request.grant = ToolGrant::TOOLS;
        let result: anyhow::Result<RunResult<serde_json::Value>> =
            agent.run(&request, "test").await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("tool calls"),
            "expected 'tool calls' in error, got: {err}"
        );
        // Round 1's 150 were executed; round 2's 51 tripped the cap before execution.
        assert_eq!(tool_calls_counter.load(Ordering::Relaxed), 150);
    }

    // -----------------------------------------------------------------------
    // finalize_result fallback path
    // -----------------------------------------------------------------------

    #[test]
    fn finalize_result_plain_text_fallback() {
        // When model output is plain text (not valid JSON), finalize_result
        // wraps it as a JSON string and re-parses. Verify this fallback path
        // produces a serde_json::Value::String containing the original text.
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![flick::ContentBlock::Text {
                text: "This is plain text, not JSON.".into(),
            }],
            usage: None,
            context_hash: None,
            error: None,
        };
        let run: RunResult<serde_json::Value> = finalize_result(&result, 0).unwrap();
        assert!(
            run.output.is_string(),
            "expected Value::String for plain text fallback, got: {:?}",
            run.output
        );
        assert_eq!(
            run.output.as_str().unwrap(),
            "This is plain text, not JSON."
        );
    }

    #[test]
    fn finalize_result_valid_json_no_fallback() {
        // When model output is valid JSON, finalize_result parses it directly.
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![flick::ContentBlock::Text {
                text: r#"{"key": "value"}"#.into(),
            }],
            usage: None,
            context_hash: None,
            error: None,
        };
        let run: RunResult<serde_json::Value> = finalize_result(&result, 5).unwrap();
        assert!(run.output.is_object());
        assert_eq!(run.output["key"], "value");
        assert_eq!(run.tool_calls, 5);
    }

    #[test]
    fn finalize_result_empty_content_errors() {
        // extract_text returns Err when no text block is present.
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![],
            usage: None,
            context_hash: None,
            error: None,
        };
        let err = finalize_result::<serde_json::Value>(&result, 0);
        assert!(err.is_err(), "empty content should produce an error");
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("no text block"),
            "error should mention missing text block, got: {msg}"
        );
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct StrictStruct {
        required_field: String,
    }

    #[test]
    fn finalize_result_fallback_fails_for_concrete_type() {
        // Plain text cannot deserialize into a concrete struct even after
        // the JSON-string fallback wrapping.
        let result = flick::FlickResult {
            status: ResultStatus::Complete,
            content: vec![flick::ContentBlock::Text {
                text: "not a StrictStruct".into(),
            }],
            usage: None,
            context_hash: None,
            error: None,
        };
        let err = finalize_result::<StrictStruct>(&result, 0);
        assert!(
            err.is_err(),
            "plain text should fail to deserialize into StrictStruct"
        );
    }
}
