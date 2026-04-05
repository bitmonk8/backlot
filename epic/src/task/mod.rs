// ProblemSolverTask: assess -> execute (leaf or branch) -> verify.

pub mod assess;
pub mod branch;
pub mod leaf;
pub mod node_impl;
pub mod scope;
pub mod verify;

// Re-export orchestration-protocol types from cue.
pub use cue::{
    Attempt, LeafResult, Magnitude, MagnitudeEstimate, Model, RecoveryEligibility, RecoveryPlan,
    RegistrationInfo, ResumePoint, SessionMeta, TaskId, TaskOutcome, TaskPath, TaskPhase,
    TaskUsage,
};

use crate::agent::AgentService;
use crate::config::project::LimitsConfig;
use crate::events::EventSender;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Non-serializable runtime dependencies injected into tasks at construction
/// or after deserialization via `bind_runtime()`.
pub struct TaskRuntime<A: AgentService> {
    pub agent: Arc<A>,
    pub events: EventSender,
    pub vault: Option<Arc<vault::Vault>>,
    pub limits: LimitsConfig,
    pub project_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct Task {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub goal: String,
    pub verification_criteria: Vec<String>,
    pub path: Option<TaskPath>,
    pub phase: TaskPhase,
    pub current_model: Option<Model>,
    pub attempts: Vec<Attempt>,
    pub subtask_ids: Vec<TaskId>,
    pub magnitude_estimate: Option<MagnitudeEstimate>,
    pub magnitude: Option<Magnitude>,
    pub discoveries: Vec<String>,
    pub checkpoint_guidance: Option<String>,
    pub fix_attempts: Vec<Attempt>,
    pub decomposition_rationale: Option<String>,
    pub depth: u32,
    pub verification_fix_rounds: u32,
    pub is_fix_task: bool,
    pub recovery_rounds: u32,
    #[serde(default)]
    pub usage: TaskUsage,
}

impl Task {
    pub const fn new(
        id: TaskId,
        parent_id: Option<TaskId>,
        goal: String,
        verification_criteria: Vec<String>,
        depth: u32,
    ) -> Self {
        Self {
            id,
            parent_id,
            goal,
            verification_criteria,
            path: None,
            phase: TaskPhase::Pending,
            current_model: None,
            attempts: Vec::new(),
            subtask_ids: Vec::new(),
            magnitude_estimate: None,
            magnitude: None,
            discoveries: Vec::new(),
            checkpoint_guidance: None,
            fix_attempts: Vec::new(),
            decomposition_rationale: None,
            depth,
            verification_fix_rounds: 0,
            is_fix_task: false,
            recovery_rounds: 0,
            usage: TaskUsage::zero(),
        }
    }

    pub const fn set_assessment(
        &mut self,
        path: TaskPath,
        model: Model,
        magnitude: Option<Magnitude>,
    ) {
        self.path = Some(path);
        self.current_model = Some(model);
        self.magnitude = magnitude;
    }

    pub fn record_attempt(&mut self, attempt: Attempt, is_fix: bool) {
        if is_fix {
            self.fix_attempts.push(attempt);
        } else {
            self.attempts.push(attempt);
        }
    }

    /// Extend task discoveries and return the count added.
    pub fn record_discoveries(&mut self, discoveries: Vec<String>) -> usize {
        let count = discoveries.len();
        self.discoveries.extend(discoveries);
        count
    }

    pub const fn set_model(&mut self, model: Model) {
        self.current_model = Some(model);
    }

    pub fn set_decomposition_rationale(&mut self, rationale: String) {
        self.decomposition_rationale = Some(rationale);
    }

    pub fn set_checkpoint_guidance(&mut self, guidance: Option<String>) {
        self.checkpoint_guidance = guidance;
    }

    pub fn append_checkpoint_guidance(&mut self, new_guidance: &str) {
        self.checkpoint_guidance = Some(self.checkpoint_guidance.take().map_or_else(
            || new_guidance.to_owned(),
            |existing| format!("{existing}\n{new_guidance}"),
        ));
    }

    pub const fn increment_fix_rounds(&mut self) -> u32 {
        self.verification_fix_rounds += 1;
        self.verification_fix_rounds
    }

    pub const fn increment_recovery_rounds(&mut self) -> u32 {
        self.recovery_rounds += 1;
        self.recovery_rounds
    }

    pub fn accumulate_usage(&mut self, meta: &cue::SessionMeta) {
        self.usage.accumulate(
            meta.input_tokens,
            meta.output_tokens,
            meta.cache_creation_input_tokens,
            meta.cache_read_input_tokens,
            meta.cost_usd,
            meta.tool_calls,
            meta.total_latency_ms,
        );
    }

    /// Count consecutive trailing attempts at the given model tier.
    pub fn trailing_attempts_at_tier(&self, model: Model, is_fix: bool) -> u32 {
        let list = if is_fix {
            &self.fix_attempts
        } else {
            &self.attempts
        };
        #[allow(clippy::cast_possible_truncation)]
        let count = list.iter().rev().take_while(|a| a.model == model).count() as u32;
        count
    }

    /// Build a TaskContext from this task and a TreeContext snapshot.
    #[allow(dead_code)] // Used by legacy orchestrator retained for test migration
    pub fn to_task_context(
        &self,
        tree: &crate::orchestrator::context::TreeContext,
    ) -> crate::agent::TaskContext {
        crate::orchestrator::context::tree_to_task_context(tree, self)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.phase, TaskPhase::Completed | TaskPhase::Failed)
    }

    pub fn resume_point(&self) -> ResumePoint {
        match self.phase {
            TaskPhase::Completed => return ResumePoint::Terminal(TaskOutcome::Success),
            TaskPhase::Failed => {
                return ResumePoint::Terminal(TaskOutcome::Failed {
                    reason: "previously failed".into(),
                });
            }
            _ => {}
        }
        match (&self.path, self.phase) {
            (Some(TaskPath::Leaf), TaskPhase::Verifying) => ResumePoint::LeafVerifying,
            (Some(TaskPath::Branch), TaskPhase::Verifying) => ResumePoint::BranchVerifying,
            (Some(TaskPath::Leaf), TaskPhase::Executing) => ResumePoint::LeafExecuting,
            (Some(TaskPath::Branch), TaskPhase::Executing) => ResumePoint::BranchExecuting,
            _ => ResumePoint::NeedAssessment,
        }
    }

    pub fn forced_assessment(&self, max_depth: u32) -> Option<assess::AssessmentResult> {
        if self.parent_id.is_none() {
            return Some(assess::AssessmentResult {
                path: TaskPath::Branch,
                model: Model::Sonnet,
                rationale: "Root task always branches".into(),
                magnitude: None,
            });
        }
        if self.depth >= max_depth {
            return Some(assess::AssessmentResult {
                path: TaskPath::Leaf,
                model: Model::Sonnet,
                rationale: "Depth cap reached, forced to leaf".into(),
                magnitude: None,
            });
        }
        None
    }

    pub fn needs_decomposition(&self) -> bool {
        self.subtask_ids.is_empty()
    }

    pub fn decompose_model(&self) -> Model {
        self.current_model.unwrap_or(Model::Sonnet)
    }

    pub fn registration_info(&self) -> RegistrationInfo {
        RegistrationInfo {
            parent_id: self.parent_id,
            goal: self.goal.clone(),
            depth: self.depth,
            phase: self.phase,
        }
    }

    pub fn can_attempt_recovery(
        &self,
        limits: &crate::config::project::LimitsConfig,
    ) -> RecoveryEligibility {
        if self.is_fix_task {
            return RecoveryEligibility::NotEligible {
                reason: "fix tasks cannot recover".into(),
            };
        }
        if self.recovery_rounds >= limits.max_recovery_rounds {
            return RecoveryEligibility::NotEligible {
                reason: format!("recovery rounds exhausted ({})", limits.max_recovery_rounds),
            };
        }
        RecoveryEligibility::Eligible {
            round: self.recovery_rounds + 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_new_defaults() {
        let t = Task::new(TaskId(1), None, String::new(), Vec::new(), 0);
        assert_eq!(t.id, TaskId(1));
        assert_eq!(t.parent_id, None);
        assert_eq!(t.phase, TaskPhase::Pending);
        assert_eq!(t.path, None);
        assert_eq!(t.current_model, None);
        assert!(t.attempts.is_empty());
        assert!(t.subtask_ids.is_empty());
        assert_eq!(t.magnitude_estimate, None);
        assert_eq!(t.depth, 0);
        assert_eq!(t.verification_fix_rounds, 0);
        assert!(!t.is_fix_task);
        assert_eq!(t.recovery_rounds, 0);
    }
}
