// Flick configuration generation: YAML config files, wire format types, output schemas.

use crate::agent::models::{default_max_tokens, flick_model_id};
use crate::agent::tools::{FlickToolDef, ToolGrant, tool_definitions};
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult, SubtaskSpec};
use crate::task::verify::{VerificationOutcome, VerificationResult};
use crate::task::{LeafResult, MagnitudeEstimate, Model, TaskOutcome, TaskPath};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Flick config structs (serialized to YAML)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct FlickConfig {
    pub system_prompt: String,
    pub model: FlickModelConfig,
    pub provider: FlickProviderConfig,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<FlickToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonValue>,
}

#[derive(Debug, Serialize)]
pub struct FlickModelConfig {
    pub provider: String,
    pub name: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct FlickProviderConfig {
    pub name: String,
    pub credential: String,
}

// ---------------------------------------------------------------------------
// Wire format types (LLM-friendly flat JSON ↔ domain types)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct AssessmentWire {
    pub path: String,
    pub model: String,
    pub rationale: String,
}

impl TryFrom<AssessmentWire> for AssessmentResult {
    type Error = anyhow::Error;
    fn try_from(w: AssessmentWire) -> anyhow::Result<Self> {
        let path = match w.path.as_str() {
            "leaf" => TaskPath::Leaf,
            "branch" => TaskPath::Branch,
            _ => bail!("invalid assessment path: {}", w.path),
        };
        let model = parse_model_name(&w.model)?;
        Ok(Self {
            path,
            model,
            rationale: w.rationale,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubtaskWire {
    pub goal: String,
    pub verification_criteria: Vec<String>,
    pub magnitude: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DecompositionWire {
    pub subtasks: Vec<SubtaskWire>,
    pub rationale: String,
}

impl TryFrom<DecompositionWire> for DecompositionResult {
    type Error = anyhow::Error;
    fn try_from(w: DecompositionWire) -> anyhow::Result<Self> {
        let subtasks = w
            .subtasks
            .into_iter()
            .map(|s| {
                let magnitude = match s.magnitude.as_str() {
                    "small" => MagnitudeEstimate::Small,
                    "medium" => MagnitudeEstimate::Medium,
                    "large" => MagnitudeEstimate::Large,
                    other => bail!("invalid magnitude: {other}"),
                };
                Ok(SubtaskSpec {
                    goal: s.goal,
                    verification_criteria: s.verification_criteria,
                    magnitude_estimate: magnitude,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            subtasks,
            rationale: w.rationale,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskOutcomeWire {
    pub outcome: String,
    pub reason: Option<String>,
    pub discoveries: Option<Vec<String>>,
}

impl TryFrom<TaskOutcomeWire> for LeafResult {
    type Error = anyhow::Error;
    fn try_from(w: TaskOutcomeWire) -> anyhow::Result<Self> {
        let outcome = match w.outcome.as_str() {
            "success" => TaskOutcome::Success,
            "failed" => TaskOutcome::Failed {
                reason: w.reason.unwrap_or_else(|| "no reason provided".into()),
            },
            other => bail!("invalid task outcome: {other}"),
        };
        Ok(Self {
            outcome,
            discoveries: w.discoveries.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerificationWire {
    pub outcome: String,
    pub reason: Option<String>,
    pub details: String,
}

impl TryFrom<VerificationWire> for VerificationResult {
    type Error = anyhow::Error;
    fn try_from(w: VerificationWire) -> anyhow::Result<Self> {
        let outcome = match w.outcome.as_str() {
            "pass" => VerificationOutcome::Pass,
            "fail" => VerificationOutcome::Fail {
                reason: w.reason.unwrap_or_else(|| "no reason provided".into()),
            },
            other => bail!("invalid verification outcome: {other}"),
        };
        Ok(Self {
            outcome,
            details: w.details,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckpointWire {
    pub decision: String,
    pub guidance: Option<String>,
}

impl TryFrom<CheckpointWire> for CheckpointDecision {
    type Error = anyhow::Error;
    fn try_from(w: CheckpointWire) -> anyhow::Result<Self> {
        match w.decision.as_str() {
            "proceed" => Ok(Self::Proceed),
            "adjust" => Ok(Self::Adjust {
                guidance: w.guidance.unwrap_or_default(),
            }),
            "escalate" => Ok(Self::Escalate),
            other => bail!("invalid checkpoint decision: {other}"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryWire {
    pub recoverable: bool,
    pub strategy: Option<String>,
}

impl From<RecoveryWire> for Option<String> {
    fn from(w: RecoveryWire) -> Self {
        if w.recoverable { Some(w.strategy.unwrap_or_default()) } else { None }
    }
}

// ---------------------------------------------------------------------------
// Output schema generators
// ---------------------------------------------------------------------------

pub fn assessment_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "enum": ["leaf", "branch"] },
            "model": { "type": "string", "enum": ["haiku", "sonnet", "opus"] },
            "rationale": { "type": "string" }
        },
        "required": ["path", "model", "rationale"]
    })
}

pub fn decomposition_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "subtasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "goal": { "type": "string" },
                        "verification_criteria": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "magnitude": { "type": "string", "enum": ["small", "medium", "large"] }
                    },
                    "required": ["goal", "verification_criteria", "magnitude"]
                }
            },
            "rationale": { "type": "string" }
        },
        "required": ["subtasks", "rationale"]
    })
}

pub fn task_outcome_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["success", "failed"] },
            "reason": { "type": "string" },
            "discoveries": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["outcome"]
    })
}

pub fn verification_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "outcome": { "type": "string", "enum": ["pass", "fail"] },
            "reason": { "type": "string" },
            "details": { "type": "string" }
        },
        "required": ["outcome", "details"]
    })
}

pub fn checkpoint_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "decision": { "type": "string", "enum": ["proceed", "adjust", "escalate"] },
            "guidance": { "type": "string" }
        },
        "required": ["decision"]
    })
}

pub fn recovery_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "recoverable": { "type": "boolean" },
            "strategy": { "type": "string" }
        },
        "required": ["recoverable"]
    })
}

// ---------------------------------------------------------------------------
// Config builders (one per AgentService method)
// ---------------------------------------------------------------------------

fn base_config(
    system_prompt: String,
    model: Model,
    credential_name: &str,
) -> FlickConfig {
    FlickConfig {
        system_prompt,
        model: FlickModelConfig {
            provider: "anthropic".into(),
            name: flick_model_id(model).into(),
            max_tokens: default_max_tokens(model),
            temperature: Some(0.0),
        },
        provider: FlickProviderConfig {
            name: "anthropic".into(),
            credential: credential_name.into(),
        },
        tools: Vec::new(),
        output_schema: None,
    }
}

pub fn build_assess_config(
    system_prompt: String,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> FlickConfig {
    let mut cfg = base_config(system_prompt, model, credential);
    cfg.tools = tool_definitions(grant);
    cfg.output_schema = Some(assessment_schema());
    cfg
}

pub fn build_execute_leaf_config(
    system_prompt: String,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> FlickConfig {
    let mut cfg = base_config(system_prompt, model, credential);
    cfg.tools = tool_definitions(grant);
    cfg.output_schema = Some(task_outcome_schema());
    cfg
}

pub fn build_decompose_config(
    system_prompt: String,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> FlickConfig {
    let mut cfg = base_config(system_prompt, model, credential);
    cfg.tools = tool_definitions(grant);
    cfg.output_schema = Some(decomposition_schema());
    cfg
}

pub fn build_verify_config(
    system_prompt: String,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> FlickConfig {
    let mut cfg = base_config(system_prompt, model, credential);
    cfg.tools = tool_definitions(grant);
    cfg.output_schema = Some(verification_schema());
    cfg
}

pub fn build_checkpoint_config(
    system_prompt: String,
    model: Model,
    credential: &str,
) -> FlickConfig {
    let mut cfg = base_config(system_prompt, model, credential);
    cfg.output_schema = Some(checkpoint_schema());
    cfg
}

pub fn build_recovery_config(
    system_prompt: String,
    model: Model,
    credential: &str,
) -> FlickConfig {
    let mut cfg = base_config(system_prompt, model, credential);
    cfg.output_schema = Some(recovery_schema());
    cfg
}

// ---------------------------------------------------------------------------
// Write config to disk
// ---------------------------------------------------------------------------

/// Writes a Flick config to `{work_dir}/{task_id}_{method}.yaml` and returns the path.
pub async fn write_config(
    config: &FlickConfig,
    work_dir: &Path,
    task_id: u64,
    method: &str,
) -> anyhow::Result<PathBuf> {
    let filename = format!("{task_id}_{method}.yaml");
    let path = work_dir.join(filename);
    let yaml = serde_yaml::to_string(config).context("failed to serialize flick config")?;
    tokio::fs::write(&path, yaml).await.with_context(|| format!("failed to write config to {}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn parse_model_name(s: &str) -> anyhow::Result<Model> {
    match s {
        "haiku" => Ok(Model::Haiku),
        "sonnet" => Ok(Model::Sonnet),
        "opus" => Ok(Model::Opus),
        other => bail!("invalid model name: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assessment_wire_roundtrip() {
        let wire = AssessmentWire {
            path: "leaf".into(),
            model: "haiku".into(),
            rationale: "simple".into(),
        };
        let result = AssessmentResult::try_from(wire).unwrap();
        assert_eq!(result.path, TaskPath::Leaf);
        assert_eq!(result.model, Model::Haiku);
    }

    #[test]
    fn assessment_wire_invalid_path() {
        let wire = AssessmentWire {
            path: "invalid".into(),
            model: "haiku".into(),
            rationale: "x".into(),
        };
        assert!(AssessmentResult::try_from(wire).is_err());
    }

    #[test]
    fn decomposition_wire_roundtrip() {
        let wire = DecompositionWire {
            subtasks: vec![SubtaskWire {
                goal: "do thing".into(),
                verification_criteria: vec!["it works".into()],
                magnitude: "small".into(),
            }],
            rationale: "because".into(),
        };
        let result = DecompositionResult::try_from(wire).unwrap();
        assert_eq!(result.subtasks.len(), 1);
        assert_eq!(result.subtasks[0].magnitude_estimate, MagnitudeEstimate::Small);
    }

    #[test]
    fn task_outcome_wire_success() {
        let wire = TaskOutcomeWire {
            outcome: "success".into(),
            reason: None,
            discoveries: None,
        };
        let result = LeafResult::try_from(wire).unwrap();
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert!(result.discoveries.is_empty());
    }

    #[test]
    fn task_outcome_wire_failed() {
        let wire = TaskOutcomeWire {
            outcome: "failed".into(),
            reason: Some("broke".into()),
            discoveries: None,
        };
        let result = LeafResult::try_from(wire).unwrap();
        assert!(matches!(result.outcome, TaskOutcome::Failed { reason } if reason == "broke"));
    }

    #[test]
    fn task_outcome_wire_with_discoveries() {
        let wire = TaskOutcomeWire {
            outcome: "success".into(),
            reason: None,
            discoveries: Some(vec!["found API v2".into(), "cache layer exists".into()]),
        };
        let result = LeafResult::try_from(wire).unwrap();
        assert_eq!(result.outcome, TaskOutcome::Success);
        assert_eq!(result.discoveries, vec!["found API v2", "cache layer exists"]);
    }

    #[test]
    fn verification_wire_pass() {
        let wire = VerificationWire {
            outcome: "pass".into(),
            reason: None,
            details: "all good".into(),
        };
        let result = VerificationResult::try_from(wire).unwrap();
        assert_eq!(result.outcome, VerificationOutcome::Pass);
    }

    #[test]
    fn checkpoint_wire_variants() {
        let proceed = CheckpointWire {
            decision: "proceed".into(),
            guidance: None,
        };
        assert_eq!(
            CheckpointDecision::try_from(proceed).unwrap(),
            CheckpointDecision::Proceed
        );

        let adjust = CheckpointWire {
            decision: "adjust".into(),
            guidance: Some("do X".into()),
        };
        assert_eq!(
            CheckpointDecision::try_from(adjust).unwrap(),
            CheckpointDecision::Adjust {
                guidance: "do X".into()
            }
        );
    }

    #[test]
    fn recovery_wire_conversion() {
        let recoverable = RecoveryWire {
            recoverable: true,
            strategy: Some("retry with different approach".into()),
        };
        let result: Option<String> = recoverable.into();
        assert_eq!(result, Some("retry with different approach".into()));

        let not_recoverable = RecoveryWire {
            recoverable: false,
            strategy: None,
        };
        let result: Option<String> = not_recoverable.into();
        assert!(result.is_none());

        // Edge case: recoverable=true but strategy=None should still signal recoverability.
        let recoverable_no_strategy = RecoveryWire {
            recoverable: true,
            strategy: None,
        };
        let result: Option<String> = recoverable_no_strategy.into();
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn config_serializes_to_yaml() {
        let cfg = build_assess_config(
            "You are an assessor.".into(),
            Model::Sonnet,
            "anthropic_key",
            ToolGrant::READ,
        );
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(yaml.contains("claude-sonnet-4-6"));
        assert!(yaml.contains("anthropic_key"));
        assert!(yaml.contains("output_schema"));
        assert!(yaml.contains("read_file"));
    }

    #[tokio::test]
    async fn write_config_creates_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        let cfg = build_assess_config(
            "test".into(),
            Model::Haiku,
            "key",
            ToolGrant::empty(),
        );
        let path = write_config(&cfg, dir, 42, "assess").await.unwrap();
        assert!(path.exists());
        assert!(path.file_name().unwrap().to_str().unwrap().contains("42_assess"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("claude-haiku"));
    }
}
