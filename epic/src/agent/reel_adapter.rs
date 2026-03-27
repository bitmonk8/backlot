// ReelAgent: AgentService implementation backed by reel::Agent.
//
// Thin adapter: builds prompts and wire types (epic-specific), delegates
// tool loop and tool execution to reel.

use crate::agent::prompts;
use crate::agent::wire::{
    self, AssessmentWire, CheckpointWire, DecompositionWire, RecoveryPlanWire, RecoveryWire,
    TaskOutcomeWire, VerificationWire,
};
use crate::agent::{AgentResult, AgentService, SessionMeta, TaskContext};
use crate::config::project::{ModelConfig, VerificationStep};
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{LeafResult, Model, RecoveryPlan};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Phase → grant mapping (epic-specific, stays here)
// ---------------------------------------------------------------------------

/// Returns the tool grant appropriate for execute (leaf/fix-leaf) phases.
const fn execute_grant() -> reel::ToolGrant {
    reel::ToolGrant::WRITE.union(reel::ToolGrant::TOOLS)
}

/// Returns the tool grant appropriate for read-only phases (verify, decompose, design).
const fn readonly_grant() -> reel::ToolGrant {
    reel::ToolGrant::TOOLS
}

// ---------------------------------------------------------------------------
// Model tier helpers
// ---------------------------------------------------------------------------

/// Default max token budget for a given model tier.
pub const fn default_max_tokens(model: Model) -> u32 {
    match model {
        Model::Haiku | Model::Sonnet => 8192,
        Model::Opus => 16384,
    }
}

/// Model registry key for a given tier.
pub const fn model_key(model: Model) -> &'static str {
    match model {
        Model::Haiku => "fast",
        Model::Sonnet => "balanced",
        Model::Opus => "strong",
    }
}

// ---------------------------------------------------------------------------
// Model registry
// ---------------------------------------------------------------------------

/// Build a `ModelRegistry` from epic's `ModelConfig` and credential name.
fn build_model_registry(
    model_config: &ModelConfig,
    credential_name: &str,
) -> anyhow::Result<reel::ModelRegistry> {
    let mut map = BTreeMap::new();
    for tier in [Model::Haiku, Model::Sonnet, Model::Opus] {
        map.insert(
            model_key(tier).to_string(),
            reel::ModelInfo {
                provider: credential_name.to_string(),
                name: model_config.name_for(tier).to_string(),
                max_tokens: Some(default_max_tokens(tier)),
                input_per_million: None,
                output_per_million: None,
                cache_creation_per_million: None,
                cache_read_per_million: None,
            },
        );
    }
    reel::ModelRegistry::from_map(map)
        .map_err(|e| anyhow::anyhow!("failed to build model registry: {e}"))
}

// ---------------------------------------------------------------------------
// ReelAgent
// ---------------------------------------------------------------------------

/// `ReelAgent` delegates to `reel::Agent` for tool loop execution.
pub struct ReelAgent {
    agent: reel::Agent,
    verification_steps: Vec<VerificationStep>,
}

impl ReelAgent {
    pub fn new(
        project_root: PathBuf,
        credential_name: &str,
        call_timeout: Duration,
        model_config: &ModelConfig,
        verification_steps: Vec<VerificationStep>,
    ) -> anyhow::Result<Self> {
        let model_registry = build_model_registry(model_config, credential_name)?;
        let provider_registry = reel::ProviderRegistry::load_default()
            .map_err(|e| anyhow::anyhow!("failed to load provider registry: {e}"))?;

        let env = reel::AgentEnvironment {
            model_registry,
            provider_registry,
            project_root,
            timeout: call_timeout,
        };

        Ok(Self {
            agent: reel::Agent::new(env),
            verification_steps,
        })
    }

    /// Run an agent call with the given grant (empty grant = structured, no tools).
    async fn run_request<T: DeserializeOwned>(
        &self,
        system_prompt: &str,
        query: &str,
        model: Model,
        grant: reel::ToolGrant,
        output_schema: serde_json::Value,
    ) -> anyhow::Result<AgentResult<T>> {
        let config = reel::RequestConfig::builder()
            .model(model_key(model))
            .system_prompt(system_prompt)
            .output_schema(output_schema)
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build request config: {e}"))?;

        let request = reel::AgentRequestConfig {
            config,
            grant,
            custom_tools: Vec::new(),
            write_paths: Vec::new(),
        };
        let result: reel::RunResult<T> = self.agent.run(&request, query).await?;
        let meta = SessionMeta::from_run_result(&result);
        Ok(AgentResult {
            value: result.output,
            meta,
        })
    }

    async fn run_leaf_task(
        &self,
        pair: prompts::PromptPair,
        model: Model,
    ) -> anyhow::Result<AgentResult<LeafResult>> {
        let grant = execute_grant();
        let schema = wire::task_outcome_schema();
        let AgentResult { value: wire, meta }: AgentResult<TaskOutcomeWire> = self
            .run_request(&pair.system_prompt, &pair.query, model, grant, schema)
            .await?;
        Ok(AgentResult {
            value: LeafResult::try_from(wire)?,
            meta,
        })
    }

    async fn decompose_with_prompt(
        &self,
        pair: &prompts::PromptPair,
        model: Model,
    ) -> anyhow::Result<AgentResult<DecompositionResult>> {
        let schema = wire::decomposition_schema();
        let AgentResult { value: wire, meta }: AgentResult<DecompositionWire> = self
            .run_request(
                &pair.system_prompt,
                &pair.query,
                model,
                readonly_grant(),
                schema,
            )
            .await?;
        Ok(AgentResult {
            value: DecompositionResult::try_from(wire)?,
            meta,
        })
    }
}

// ---------------------------------------------------------------------------
// Init exploration (not part of AgentService — standalone call)
// ---------------------------------------------------------------------------

impl ReelAgent {
    /// Run the init exploration agent to detect project build/test/lint setup.
    pub async fn explore_for_init(&self) -> anyhow::Result<AgentResult<wire::InitFindingsWire>> {
        let pair = prompts::build_explore_for_init();
        let schema = wire::init_findings_schema();
        self.run_request(
            &pair.system_prompt,
            &pair.query,
            Model::Sonnet,
            readonly_grant(),
            schema,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// AgentService implementation
// ---------------------------------------------------------------------------

impl AgentService for ReelAgent {
    async fn assess(&self, ctx: &TaskContext) -> anyhow::Result<AgentResult<AssessmentResult>> {
        let pair = prompts::build_assess(ctx);
        let schema = wire::assessment_schema();
        let AgentResult { value: wire, meta }: AgentResult<AssessmentWire> = self
            .run_request(
                &pair.system_prompt,
                &pair.query,
                Model::Haiku,
                reel::ToolGrant::empty(),
                schema,
            )
            .await?;
        Ok(AgentResult {
            value: AssessmentResult::try_from(wire)?,
            meta,
        })
    }

    async fn execute_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<AgentResult<LeafResult>> {
        let pair = prompts::build_execute_leaf(ctx);
        self.run_leaf_task(pair, model).await
    }

    async fn fix_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
        failure_reason: &str,
        attempt: u32,
    ) -> anyhow::Result<AgentResult<LeafResult>> {
        let pair = prompts::build_fix_leaf(ctx, failure_reason, attempt);
        self.run_leaf_task(pair, model).await
    }

    async fn design_and_decompose(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<AgentResult<DecompositionResult>> {
        let pair = prompts::build_design_and_decompose(ctx);
        self.decompose_with_prompt(&pair, model).await
    }

    async fn design_fix_subtasks(
        &self,
        ctx: &TaskContext,
        model: Model,
        verification_issues: &str,
        round: u32,
    ) -> anyhow::Result<AgentResult<DecompositionResult>> {
        let pair = prompts::build_design_fix_subtasks(ctx, verification_issues, round);
        self.decompose_with_prompt(&pair, model).await
    }

    async fn verify(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<AgentResult<VerificationResult>> {
        let pair = prompts::build_verify(ctx, &self.verification_steps);
        let schema = wire::verification_schema();
        let AgentResult { value: wire, meta }: AgentResult<VerificationWire> = self
            .run_request(
                &pair.system_prompt,
                &pair.query,
                model,
                readonly_grant(),
                schema,
            )
            .await?;
        Ok(AgentResult {
            value: VerificationResult::try_from(wire)?,
            meta,
        })
    }

    async fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> anyhow::Result<AgentResult<CheckpointDecision>> {
        let pair = prompts::build_checkpoint(ctx, discoveries);
        let schema = wire::checkpoint_schema();
        let AgentResult { value: wire, meta }: AgentResult<CheckpointWire> = self
            .run_request(
                &pair.system_prompt,
                &pair.query,
                Model::Haiku,
                reel::ToolGrant::empty(),
                schema,
            )
            .await?;
        Ok(AgentResult {
            value: CheckpointDecision::try_from(wire)?,
            meta,
        })
    }

    async fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> anyhow::Result<AgentResult<Option<String>>> {
        let pair = prompts::build_assess_recovery(ctx, failure_reason);
        let schema = wire::recovery_schema();
        let AgentResult { value: wire, meta }: AgentResult<RecoveryWire> = self
            .run_request(
                &pair.system_prompt,
                &pair.query,
                Model::Opus,
                reel::ToolGrant::empty(),
                schema,
            )
            .await?;
        Ok(AgentResult {
            value: wire.into_strategy(),
            meta,
        })
    }

    async fn design_recovery_subtasks(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
        strategy: &str,
        recovery_round: u32,
    ) -> anyhow::Result<AgentResult<RecoveryPlan>> {
        let pair =
            prompts::build_design_recovery_subtasks(ctx, failure_reason, strategy, recovery_round);
        let schema = wire::recovery_plan_schema();
        let AgentResult { value: wire, meta }: AgentResult<RecoveryPlanWire> = self
            .run_request(
                &pair.system_prompt,
                &pair.query,
                Model::Opus,
                readonly_grant(),
                schema,
            )
            .await?;
        Ok(AgentResult {
            value: RecoveryPlan::try_from(wire)?,
            meta,
        })
    }
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
        assert_eq!(fast.max_tokens, Some(default_max_tokens(Model::Haiku)));

        let balanced = registry.get("balanced").expect("missing 'balanced' entry");
        assert_eq!(balanced.name, "my-sonnet");
        assert_eq!(balanced.provider, "my-cred");
        assert_eq!(balanced.max_tokens, Some(default_max_tokens(Model::Sonnet)));

        let strong = registry.get("strong").expect("missing 'strong' entry");
        assert_eq!(strong.name, "my-opus");
        assert_eq!(strong.provider, "my-cred");
        assert_eq!(strong.max_tokens, Some(default_max_tokens(Model::Opus)));
    }

    // -----------------------------------------------------------------------
    // Model tier helpers
    // -----------------------------------------------------------------------

    #[test]
    fn model_key_mapping() {
        assert_eq!(model_key(Model::Haiku), "fast");
        assert_eq!(model_key(Model::Sonnet), "balanced");
        assert_eq!(model_key(Model::Opus), "strong");
    }

    #[test]
    fn default_max_tokens_per_tier() {
        assert_eq!(default_max_tokens(Model::Haiku), 8192);
        assert_eq!(default_max_tokens(Model::Sonnet), 8192);
        assert_eq!(default_max_tokens(Model::Opus), 16384);
    }

    // -----------------------------------------------------------------------
    // Grant mapping tests
    // -----------------------------------------------------------------------

    #[test]
    fn execute_grant_includes_write_and_nu() {
        let grant = execute_grant();
        assert!(grant.contains(reel::ToolGrant::WRITE));
        assert!(grant.contains(reel::ToolGrant::TOOLS));
    }

    #[test]
    fn readonly_grant_includes_nu_not_write() {
        let grant = readonly_grant();
        assert!(grant.contains(reel::ToolGrant::TOOLS));
        assert!(!grant.contains(reel::ToolGrant::WRITE));
    }
}
