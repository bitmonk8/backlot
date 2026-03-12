// Flick configuration generation: in-memory config, wire format types, output schemas.

use crate::agent::tools::{FlickToolDef, ToolGrant, tool_definitions};
use crate::config::project::VerificationStep;
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult, SubtaskSpec};
use crate::task::verify::{VerificationOutcome, VerificationResult};
use crate::task::{
    LeafResult, Magnitude, MagnitudeEstimate, Model, RecoveryPlan, TaskOutcome, TaskPath,
};
use anyhow::bail;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

// ---------------------------------------------------------------------------
// Wire format types (LLM-friendly flat JSON ↔ domain types)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct AssessmentWire {
    pub path: String,
    pub model: String,
    pub rationale: String,
    pub max_lines_added: Option<u64>,
    pub max_lines_modified: Option<u64>,
    pub max_lines_deleted: Option<u64>,
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
        let magnitude = match (w.max_lines_added, w.max_lines_modified, w.max_lines_deleted) {
            (None, None, None) => None,
            (added, modified, deleted) => Some(Magnitude {
                max_lines_added: added.unwrap_or(0),
                max_lines_modified: modified.unwrap_or(0),
                max_lines_deleted: deleted.unwrap_or(0),
            }),
        };
        Ok(Self {
            path,
            model,
            rationale: w.rationale,
            magnitude,
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

fn parse_subtask_wire(s: SubtaskWire) -> anyhow::Result<SubtaskSpec> {
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
}

impl TryFrom<DecompositionWire> for DecompositionResult {
    type Error = anyhow::Error;
    fn try_from(w: DecompositionWire) -> anyhow::Result<Self> {
        if w.subtasks.is_empty() {
            bail!("decomposition must contain at least one subtask");
        }
        let subtasks = w
            .subtasks
            .into_iter()
            .map(parse_subtask_wire)
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

impl RecoveryWire {
    pub fn into_strategy(self) -> Option<String> {
        if self.recoverable {
            Some(self.strategy.unwrap_or_default())
        } else {
            None
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryPlanWire {
    pub approach: String,
    pub subtasks: Vec<SubtaskWire>,
    pub rationale: String,
}

impl TryFrom<RecoveryPlanWire> for RecoveryPlan {
    type Error = anyhow::Error;
    fn try_from(w: RecoveryPlanWire) -> anyhow::Result<Self> {
        let full_redecomposition = match w.approach.as_str() {
            "incremental" => false,
            "full" => true,
            other => bail!("invalid recovery approach: {other}"),
        };
        if w.subtasks.is_empty() {
            bail!("recovery plan must contain at least one subtask");
        }
        let subtasks = w
            .subtasks
            .into_iter()
            .map(parse_subtask_wire)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            full_redecomposition,
            subtasks,
            rationale: w.rationale,
        })
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
            "rationale": { "type": "string" },
            "max_lines_added": { "type": "integer" },
            "max_lines_modified": { "type": "integer" },
            "max_lines_deleted": { "type": "integer" }
        },
        "required": ["path", "model", "rationale"]
    })
}

fn subtask_schema() -> JsonValue {
    serde_json::json!({
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
    })
}

pub fn decomposition_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "subtasks": {
                "type": "array",
                "items": subtask_schema()
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

pub fn recovery_plan_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "approach": { "type": "string", "enum": ["incremental", "full"] },
            "subtasks": {
                "type": "array",
                "items": subtask_schema()
            },
            "rationale": { "type": "string" }
        },
        "required": ["approach", "subtasks", "rationale"]
    })
}

// ---------------------------------------------------------------------------
// Config builders (one per AgentService method)
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

fn build_config_json(
    system_prompt: &str,
    model: Model,
    tools: &[FlickToolDef],
    output_schema: &JsonValue,
) -> JsonValue {
    let mut json = serde_json::json!({
        "model": model_key(model),
        "system_prompt": system_prompt,
        "temperature": 0.0
    });

    if !tools.is_empty() {
        let tool_array: Vec<JsonValue> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                })
            })
            .collect();
        json["tools"] = JsonValue::Array(tool_array);
    }

    json["output_schema"] = serde_json::json!({ "schema": output_schema });

    json
}

fn build_config(
    system_prompt: &str,
    model: Model,
    tools: &[FlickToolDef],
    output_schema: &JsonValue,
) -> anyhow::Result<flick::RequestConfig> {
    let json = build_config_json(system_prompt, model, tools, output_schema);
    let json_str =
        serde_json::to_string(&json).map_err(|e| anyhow::anyhow!("config serialize: {e}"))?;
    flick::RequestConfig::from_str(&json_str, flick::ConfigFormat::Json)
        .map_err(|e| anyhow::anyhow!("failed to parse flick config: {e}"))
}

pub fn build_assess_config(
    system_prompt: &str,
    model: Model,
) -> anyhow::Result<flick::RequestConfig> {
    let schema = assessment_schema();
    build_config(system_prompt, model, &[], &schema)
}

pub fn build_execute_leaf_config(
    system_prompt: &str,
    model: Model,
    grant: ToolGrant,
) -> anyhow::Result<flick::RequestConfig> {
    let tools = tool_definitions(grant);
    let schema = task_outcome_schema();
    build_config(system_prompt, model, &tools, &schema)
}

pub fn build_decompose_config(
    system_prompt: &str,
    model: Model,
    grant: ToolGrant,
) -> anyhow::Result<flick::RequestConfig> {
    let tools = tool_definitions(grant);
    let schema = decomposition_schema();
    build_config(system_prompt, model, &tools, &schema)
}

pub fn build_verify_config(
    system_prompt: &str,
    model: Model,
    grant: ToolGrant,
) -> anyhow::Result<flick::RequestConfig> {
    let tools = tool_definitions(grant);
    let schema = verification_schema();
    build_config(system_prompt, model, &tools, &schema)
}

pub fn build_checkpoint_config(
    system_prompt: &str,
    model: Model,
) -> anyhow::Result<flick::RequestConfig> {
    let schema = checkpoint_schema();
    build_config(system_prompt, model, &[], &schema)
}

pub fn build_recovery_config(
    system_prompt: &str,
    model: Model,
) -> anyhow::Result<flick::RequestConfig> {
    let schema = recovery_schema();
    build_config(system_prompt, model, &[], &schema)
}

pub fn build_recovery_plan_config(
    system_prompt: &str,
    model: Model,
    grant: ToolGrant,
) -> anyhow::Result<flick::RequestConfig> {
    let tools = tool_definitions(grant);
    let schema = recovery_plan_schema();
    build_config(system_prompt, model, &tools, &schema)
}

// ---------------------------------------------------------------------------
// Init exploration types
// ---------------------------------------------------------------------------

/// Wire format for a detected verification step from the init exploration agent.
#[derive(Debug, Serialize, Deserialize)]
pub struct DetectedStepWire {
    pub name: String,
    pub command: Vec<String>,
    pub timeout: Option<u32>,
    pub rationale: String,
}

impl From<DetectedStepWire> for VerificationStep {
    fn from(w: DetectedStepWire) -> Self {
        Self {
            name: w.name,
            command: w.command,
            timeout: w.timeout.unwrap_or(300),
        }
    }
}

/// Wire format for init exploration agent output.
#[derive(Debug, Serialize, Deserialize)]
pub struct InitFindingsWire {
    pub project_type: String,
    pub steps: Vec<DetectedStepWire>,
    pub notes: Option<String>,
}

pub fn init_findings_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "properties": {
            "project_type": {
                "type": "string",
                "description": "Short description of the project type (e.g. 'Rust/Cargo', 'Node.js/npm', 'Python/poetry')"
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Step name: Build, Lint, Test, or Format"
                        },
                        "command": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Command as array of strings (e.g. [\"cargo\", \"build\"])"
                        },
                        "timeout": {
                            "type": "integer",
                            "description": "Timeout in seconds (default 300)"
                        },
                        "rationale": {
                            "type": "string",
                            "description": "Why this step was detected"
                        }
                    },
                    "required": ["name", "command", "rationale"]
                }
            },
            "notes": {
                "type": "string",
                "description": "Additional observations about the project setup"
            }
        },
        "required": ["project_type", "steps"]
    })
}

pub fn build_init_config(
    system_prompt: &str,
    grant: ToolGrant,
) -> anyhow::Result<flick::RequestConfig> {
    let tools = tool_definitions(grant);
    let schema = init_findings_schema();
    build_config(system_prompt, Model::Sonnet, &tools, &schema)
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
            max_lines_added: None,
            max_lines_modified: None,
            max_lines_deleted: None,
        };
        let result = AssessmentResult::try_from(wire).unwrap();
        assert_eq!(result.path, TaskPath::Leaf);
        assert_eq!(result.model, Model::Haiku);
        assert!(result.magnitude.is_none());
    }

    #[test]
    fn assessment_wire_invalid_path() {
        let wire = AssessmentWire {
            path: "invalid".into(),
            model: "haiku".into(),
            rationale: "x".into(),
            max_lines_added: None,
            max_lines_modified: None,
            max_lines_deleted: None,
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
        assert_eq!(
            result.subtasks[0].magnitude_estimate,
            MagnitudeEstimate::Small
        );
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
        assert_eq!(
            result.discoveries,
            vec!["found API v2", "cache layer exists"]
        );
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
        assert_eq!(
            recoverable.into_strategy(),
            Some("retry with different approach".into())
        );

        let not_recoverable = RecoveryWire {
            recoverable: false,
            strategy: None,
        };
        assert!(not_recoverable.into_strategy().is_none());

        // Edge case: recoverable=true but strategy=None should still signal recoverability.
        let recoverable_no_strategy = RecoveryWire {
            recoverable: true,
            strategy: None,
        };
        assert_eq!(recoverable_no_strategy.into_strategy(), Some(String::new()));
    }

    #[test]
    fn config_builds_with_correct_content() {
        let json = build_config_json(
            "You are an assessor.",
            Model::Sonnet,
            &[],
            &assessment_schema(),
        );

        assert_eq!(json["model"], "balanced");
        assert_eq!(json["temperature"], 0.0);
        // assess config should have no tools
        assert!(json.get("tools").is_none());
        assert!(json["output_schema"]["schema"]["properties"]["path"].is_object());

        // Verify Flick accepts it
        let config = build_assess_config("You are an assessor.", Model::Sonnet);
        assert!(config.is_ok());
    }

    #[test]
    fn config_no_tools_no_tools_key() {
        let config = build_checkpoint_config("You are a checkpoint agent.", Model::Haiku);
        assert!(config.is_ok());
    }

    #[test]
    fn assessment_wire_with_magnitude() {
        let wire = AssessmentWire {
            path: "leaf".into(),
            model: "sonnet".into(),
            rationale: "test".into(),
            max_lines_added: Some(100),
            max_lines_modified: Some(50),
            max_lines_deleted: Some(25),
        };
        let result = AssessmentResult::try_from(wire).unwrap();
        let mag = result.magnitude.unwrap();
        assert_eq!(mag.max_lines_added, 100);
        assert_eq!(mag.max_lines_modified, 50);
        assert_eq!(mag.max_lines_deleted, 25);
    }

    #[test]
    fn recovery_plan_wire_incremental() {
        let wire = RecoveryPlanWire {
            approach: "incremental".into(),
            subtasks: vec![SubtaskWire {
                goal: "fix thing".into(),
                verification_criteria: vec!["fixed".into()],
                magnitude: "small".into(),
            }],
            rationale: "targeted fix".into(),
        };
        let result = RecoveryPlan::try_from(wire).unwrap();
        assert!(!result.full_redecomposition);
        assert_eq!(result.subtasks.len(), 1);
        assert_eq!(
            result.subtasks[0].magnitude_estimate,
            MagnitudeEstimate::Small
        );
    }

    #[test]
    fn recovery_plan_wire_full() {
        let wire = RecoveryPlanWire {
            approach: "full".into(),
            subtasks: vec![SubtaskWire {
                goal: "redo A".into(),
                verification_criteria: vec!["works".into()],
                magnitude: "medium".into(),
            }],
            rationale: "wrong approach".into(),
        };
        let result = RecoveryPlan::try_from(wire).unwrap();
        assert!(result.full_redecomposition);
    }

    #[test]
    fn recovery_plan_wire_invalid_approach() {
        let wire = RecoveryPlanWire {
            approach: "partial".into(),
            subtasks: vec![SubtaskWire {
                goal: "g".into(),
                verification_criteria: vec!["c".into()],
                magnitude: "small".into(),
            }],
            rationale: "x".into(),
        };
        assert!(RecoveryPlan::try_from(wire).is_err());
    }

    #[test]
    fn model_key_mapping() {
        assert_eq!(model_key(Model::Haiku), "fast");
        assert_eq!(model_key(Model::Sonnet), "balanced");
        assert_eq!(model_key(Model::Opus), "strong");
    }

    #[test]
    fn build_init_config_uses_sonnet_tier() {
        let config = build_init_config("You are an init explorer.", ToolGrant::NU).unwrap();
        // build_init_config uses Model::Sonnet → "balanced" key
        assert_eq!(config.model(), "balanced");
    }

    #[test]
    fn default_max_tokens_per_tier() {
        assert_eq!(default_max_tokens(Model::Haiku), 8192);
        assert_eq!(default_max_tokens(Model::Sonnet), 8192);
        assert_eq!(default_max_tokens(Model::Opus), 16384);
    }

    #[test]
    fn decomposition_wire_empty_subtasks_rejected() {
        let wire = DecompositionWire {
            subtasks: vec![],
            rationale: "empty".into(),
        };
        let err = DecompositionResult::try_from(wire).unwrap_err();
        assert!(err.to_string().contains("at least one subtask"));
    }

    #[test]
    fn recovery_plan_wire_empty_subtasks_rejected() {
        let wire = RecoveryPlanWire {
            approach: "incremental".into(),
            subtasks: vec![],
            rationale: "empty".into(),
        };
        let err = RecoveryPlan::try_from(wire).unwrap_err();
        assert!(err.to_string().contains("at least one subtask"));
    }

    #[test]
    fn recovery_plan_wire_full_approach_empty_subtasks_rejected() {
        let wire = RecoveryPlanWire {
            approach: "full".into(),
            subtasks: vec![],
            rationale: "empty".into(),
        };
        let err = RecoveryPlan::try_from(wire).unwrap_err();
        assert!(err.to_string().contains("at least one subtask"));
    }

    #[test]
    fn assessment_wire_partial_magnitude() {
        let wire = AssessmentWire {
            path: "branch".into(),
            model: "opus".into(),
            rationale: "test".into(),
            max_lines_added: Some(10),
            max_lines_modified: None,
            max_lines_deleted: None,
        };
        let result = AssessmentResult::try_from(wire).unwrap();
        let mag = result.magnitude.unwrap();
        assert_eq!(mag.max_lines_added, 10);
        assert_eq!(mag.max_lines_modified, 0);
        assert_eq!(mag.max_lines_deleted, 0);
    }
}
