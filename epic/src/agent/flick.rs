// FlickAgent: AgentService implementation backed by the Flick library.

use crate::agent::config_gen::{
    self, AssessmentWire, CheckpointWire, DecompositionWire, RecoveryPlanWire, RecoveryWire,
    TaskOutcomeWire, VerificationWire,
};
use crate::agent::prompts;
use crate::agent::tools::{self, AgentMethod, ToolGrant};
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
use std::path::PathBuf;
use std::time::Duration;

const MAX_TOOL_ROUNDS: u32 = 50;

struct RedactedString(String);

impl std::fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl std::fmt::Display for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// `FlickAgent` invokes the Flick library for each agent call.
pub struct FlickAgent {
    project_root: PathBuf,
    credential_name: RedactedString,
    call_timeout: Duration,
    model_config: ModelConfig,
    verification_steps: Vec<VerificationStep>,
}

impl FlickAgent {
    pub const fn new(
        project_root: PathBuf,
        credential_name: String,
        call_timeout: Duration,
        model_config: ModelConfig,
        verification_steps: Vec<VerificationStep>,
    ) -> Self {
        Self {
            project_root,
            credential_name: RedactedString(credential_name),
            call_timeout,
            model_config,
            verification_steps,
        }
    }

    // -----------------------------------------------------------------------
    // Core: build a FlickClient from a Config
    // -----------------------------------------------------------------------

    async fn build_client(&self, config: flick::Config) -> anyhow::Result<flick::FlickClient> {
        let provider = flick::resolve_provider(&config)
            .await
            .map_err(|e| anyhow::anyhow!("failed to resolve provider: {e}"))?;
        Ok(flick::FlickClient::new(config, provider))
    }

    // -----------------------------------------------------------------------
    // run_structured: single call, no tools, parse structured output
    // -----------------------------------------------------------------------

    async fn run_structured<T: DeserializeOwned>(
        &self,
        config: flick::Config,
        query: &str,
    ) -> anyhow::Result<T> {
        let client = self.build_client(config).await?;
        let mut context = flick::Context::default();

        let result = tokio::time::timeout(self.call_timeout, client.run(query, &mut context))
            .await
            .map_err(|_| anyhow::anyhow!("flick call timed out after {:?}", self.call_timeout))?
            .map_err(|e| anyhow::anyhow!("flick call failed: {e}"))?;

        check_error(&result)?;

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
        config: flick::Config,
        query: &str,
        grant: ToolGrant,
    ) -> anyhow::Result<T> {
        let client = self.build_client(config).await?;
        let mut context = flick::Context::default();

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
                let r =
                    tools::execute_tool(id.clone(), name, input, &self.project_root, grant).await;
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
            bail!("flick tool loop exceeded {MAX_TOOL_ROUNDS} rounds");
        }

        check_error(&result)?;

        let text = extract_text(&result)?;
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse structured output: {text}"))
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

        let grant = tools::ToolGrant::READ;
        let config = config_gen::build_init_config(
            system_prompt,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        self.run_with_tools(config, query, grant).await
    }
}

// ---------------------------------------------------------------------------
// AgentService implementation
// ---------------------------------------------------------------------------

impl AgentService for FlickAgent {
    async fn assess(&self, ctx: &TaskContext) -> anyhow::Result<AssessmentResult> {
        let pair = prompts::build_assess(ctx);
        let grant = tools::phase_tools(AgentMethod::Analyze);
        let config = config_gen::build_assess_config(
            &pair.system_prompt,
            Model::Haiku,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: AssessmentWire = self.run_structured(config, &pair.query).await?;
        AssessmentResult::try_from(wire)
    }

    async fn execute_leaf(&self, ctx: &TaskContext, model: Model) -> anyhow::Result<LeafResult> {
        let pair = prompts::build_execute_leaf(ctx);
        let grant = tools::phase_tools(AgentMethod::Execute);
        let config = config_gen::build_execute_leaf_config(
            &pair.system_prompt,
            model,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: TaskOutcomeWire = self.run_with_tools(config, &pair.query, grant).await?;
        LeafResult::try_from(wire)
    }

    async fn fix_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
        failure_reason: &str,
        attempt: u32,
    ) -> anyhow::Result<LeafResult> {
        let pair = prompts::build_fix_leaf(ctx, failure_reason, attempt);
        let grant = tools::phase_tools(AgentMethod::Execute);
        let config = config_gen::build_execute_leaf_config(
            &pair.system_prompt,
            model,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: TaskOutcomeWire = self.run_with_tools(config, &pair.query, grant).await?;
        LeafResult::try_from(wire)
    }

    async fn design_and_decompose(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<DecompositionResult> {
        let pair = prompts::build_design_and_decompose(ctx);
        let grant = tools::phase_tools(AgentMethod::Decompose);
        let config = config_gen::build_decompose_config(
            &pair.system_prompt,
            model,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: DecompositionWire = self.run_with_tools(config, &pair.query, grant).await?;
        DecompositionResult::try_from(wire)
    }

    async fn design_fix_subtasks(
        &self,
        ctx: &TaskContext,
        model: Model,
        verification_issues: &str,
        round: u32,
    ) -> anyhow::Result<DecompositionResult> {
        let pair = prompts::build_design_fix_subtasks(ctx, verification_issues, round);
        let grant = tools::phase_tools(AgentMethod::Decompose);
        let config = config_gen::build_decompose_config(
            &pair.system_prompt,
            model,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: DecompositionWire = self.run_with_tools(config, &pair.query, grant).await?;
        DecompositionResult::try_from(wire)
    }

    async fn verify(&self, ctx: &TaskContext, model: Model) -> anyhow::Result<VerificationResult> {
        let pair = prompts::build_verify(ctx, &self.verification_steps);
        let grant = tools::phase_tools(AgentMethod::Analyze);
        let config = config_gen::build_verify_config(
            &pair.system_prompt,
            model,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: VerificationWire = self.run_with_tools(config, &pair.query, grant).await?;
        VerificationResult::try_from(wire)
    }

    async fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> anyhow::Result<CheckpointDecision> {
        let pair = prompts::build_checkpoint(ctx, discoveries);
        let config = config_gen::build_checkpoint_config(
            &pair.system_prompt,
            Model::Haiku,
            &self.credential_name.0,
            &self.model_config,
        )?;

        let wire: CheckpointWire = self.run_structured(config, &pair.query).await?;
        CheckpointDecision::try_from(wire)
    }

    async fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> anyhow::Result<Option<String>> {
        let pair = prompts::build_assess_recovery(ctx, failure_reason);
        let config = config_gen::build_recovery_config(
            &pair.system_prompt,
            Model::Opus,
            &self.credential_name.0,
            &self.model_config,
        )?;

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
        let config = config_gen::build_recovery_plan_config(
            &pair.system_prompt,
            Model::Opus,
            &self.credential_name.0,
            grant,
            &self.model_config,
        )?;

        let wire: RecoveryPlanWire = self.run_with_tools(config, &pair.query, grant).await?;
        RecoveryPlan::try_from(wire)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    fn redacted_string_hides_value() {
        let r = RedactedString("anthropic_key".into());
        assert_eq!(format!("{r:?}"), "[REDACTED]");
        assert_eq!(format!("{r}"), "[REDACTED]");
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
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "src/main.rs"}),
                },
            ],
            usage: None,
            context_hash: Some("abc123".into()),
            error: None,
        };
        let calls = extract_tool_calls(&result).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "tu_1");
        assert_eq!(calls[0].1, "read_file");
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
}
