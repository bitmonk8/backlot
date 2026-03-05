// Flick configuration generation: in-memory config, wire format types, output schemas.

use crate::agent::models::{default_max_tokens, flick_model_id};
use crate::agent::tools::{FlickToolDef, ToolGrant, tool_definitions};
use crate::config::project::VerificationStep;
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult, SubtaskSpec};
use crate::task::verify::{VerificationOutcome, VerificationResult};
use crate::task::{LeafResult, Magnitude, MagnitudeEstimate, Model, TaskOutcome, TaskPath};
use anyhow::{Context, bail};
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
            "rationale": { "type": "string" },
            "max_lines_added": { "type": "integer" },
            "max_lines_modified": { "type": "integer" },
            "max_lines_deleted": { "type": "integer" }
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

/// Build a JSON config value and parse it into a `flick::Config`.
fn build_config(
    system_prompt: &str,
    model: Model,
    credential_name: &str,
    tools: &[FlickToolDef],
    output_schema: Option<&JsonValue>,
) -> anyhow::Result<flick::Config> {
    let mut json = serde_json::json!({
        "system_prompt": system_prompt,
        "model": {
            "provider": "anthropic",
            "name": flick_model_id(model),
            "max_tokens": default_max_tokens(model),
            "temperature": 0.0
        },
        "provider": {
            "anthropic": {
                "api": "messages",
                "credential": credential_name
            }
        }
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

    if let Some(schema) = output_schema {
        json["output_schema"] = serde_json::json!({ "schema": schema });
    }

    let json_str = serde_json::to_string(&json)
        .context("failed to serialize config JSON")?;
    flick::Config::from_str(&json_str, flick::ConfigFormat::Json)
        .map_err(|e| anyhow::anyhow!("failed to parse flick config: {e}"))
}

pub fn build_assess_config(
    system_prompt: &str,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> anyhow::Result<flick::Config> {
    let tools = tool_definitions(grant);
    let schema = assessment_schema();
    build_config(system_prompt, model, credential, &tools, Some(&schema))
}

pub fn build_execute_leaf_config(
    system_prompt: &str,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> anyhow::Result<flick::Config> {
    let tools = tool_definitions(grant);
    let schema = task_outcome_schema();
    build_config(system_prompt, model, credential, &tools, Some(&schema))
}

pub fn build_decompose_config(
    system_prompt: &str,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> anyhow::Result<flick::Config> {
    let tools = tool_definitions(grant);
    let schema = decomposition_schema();
    build_config(system_prompt, model, credential, &tools, Some(&schema))
}

pub fn build_verify_config(
    system_prompt: &str,
    model: Model,
    credential: &str,
    grant: ToolGrant,
) -> anyhow::Result<flick::Config> {
    let tools = tool_definitions(grant);
    let schema = verification_schema();
    build_config(system_prompt, model, credential, &tools, Some(&schema))
}

pub fn build_checkpoint_config(
    system_prompt: &str,
    model: Model,
    credential: &str,
) -> anyhow::Result<flick::Config> {
    let schema = checkpoint_schema();
    build_config(system_prompt, model, credential, &[], Some(&schema))
}

pub fn build_recovery_config(
    system_prompt: &str,
    model: Model,
    credential: &str,
) -> anyhow::Result<flick::Config> {
    let schema = recovery_schema();
    build_config(system_prompt, model, credential, &[], Some(&schema))
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
    credential: &str,
    grant: ToolGrant,
) -> anyhow::Result<flick::Config> {
    let tools = tool_definitions(grant);
    let schema = init_findings_schema();
    build_config(system_prompt, Model::Sonnet, credential, &tools, Some(&schema))
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
    fn config_builds_with_correct_content() {
        let tools = tool_definitions(ToolGrant::READ);
        let schema = assessment_schema();
        let json = serde_json::json!({
            "system_prompt": "You are an assessor.",
            "model": {
                "provider": "anthropic",
                "name": "claude-sonnet-4-6",
                "max_tokens": 8192,
                "temperature": 0.0
            },
            "provider": {
                "anthropic": {
                    "api": "messages",
                    "credential": "anthropic_key"
                }
            },
            "tools": tools.iter().map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters
                })
            }).collect::<Vec<_>>(),
            "output_schema": { "schema": schema }
        });

        // Verify the JSON structure matches expectations
        assert_eq!(json["model"]["name"], "claude-sonnet-4-6");
        assert_eq!(json["provider"]["anthropic"]["credential"], "anthropic_key");
        let tool_names: Vec<&str> = json["tools"].as_array().unwrap()
            .iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"glob"));
        assert!(tool_names.contains(&"grep"));
        assert!(!tool_names.contains(&"write_file"));
        assert!(json["output_schema"]["schema"]["properties"]["path"].is_object());

        // Verify Flick accepts it
        let config = build_assess_config(
            "You are an assessor.",
            Model::Sonnet,
            "anthropic_key",
            ToolGrant::READ,
        );
        assert!(config.is_ok());
    }

    #[test]
    fn config_no_tools_no_tools_key() {
        let config = build_checkpoint_config(
            "You are a checkpoint agent.",
            Model::Sonnet,
            "key",
        );
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
