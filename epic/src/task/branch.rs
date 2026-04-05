// Branch execution path: design + decompose -> execute children -> verify aggregate.
// Decision logic and self-contained operations live here; cross-task coordination
// (child execution, subtask creation, sibling state mutation) stays in the orchestrator.

use crate::agent::AgentService;
use crate::config::project::LimitsConfig;
use crate::events::Event;
use crate::orchestrator::OrchestratorError;
use crate::orchestrator::context::TreeContext;
use crate::orchestrator::services::Services;
use crate::task::scope::{self, ScopeCheck};
use crate::task::verify::VerificationOutcome;
use crate::task::{Model, Task};

// Re-export orchestration-protocol types from cue.
pub use cue::{
    BranchVerifyOutcome, CheckpointDecision, ChildResponse, DecompositionResult, FixBudgetCheck,
    RecoveryDecision, SubtaskSpec,
};

impl Task {
    /// Verify a branch task. Calls `svc.agent.verify()` with context,
    /// accumulates usage, returns structured outcome.
    pub async fn verify_branch<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> Result<BranchVerifyOutcome, OrchestratorError> {
        let verify_model = self.verification_model();
        let ctx = self.to_task_context(tree);
        let agent_result = svc.agent.verify(&ctx, verify_model).await?;
        self.accumulate_usage(&agent_result.meta);
        self.emit_usage_event(svc);

        match agent_result.value.outcome {
            VerificationOutcome::Pass => Ok(BranchVerifyOutcome::Passed),
            VerificationOutcome::Fail { reason } => {
                if self.is_fix_task {
                    Ok(BranchVerifyOutcome::FailedNoFixLoop { reason })
                } else {
                    Ok(BranchVerifyOutcome::Failed { reason })
                }
            }
        }
    }

    /// Check whether the fix round budget is exhausted.
    pub const fn fix_round_budget_check(&self, limits: &LimitsConfig) -> FixBudgetCheck {
        let is_root = self.parent_id.is_none();
        let max_rounds = if is_root {
            limits.root_fix_rounds
        } else {
            limits.branch_fix_rounds
        };
        if self.verification_fix_rounds >= max_rounds {
            return FixBudgetCheck::Exhausted;
        }
        let next_round = self.verification_fix_rounds + 1;
        let model = if next_round <= 3 {
            Model::Sonnet
        } else {
            Model::Opus
        };
        FixBudgetCheck::WithinBudget { model }
    }

    /// Design fix subtasks to address branch verification issues.
    pub async fn design_fix<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
        failure: &str,
        round: u32,
        model: Model,
    ) -> Result<Result<Vec<SubtaskSpec>, String>, OrchestratorError> {
        let ctx = self.to_task_context(tree);
        match svc
            .agent
            .design_fix_subtasks(&ctx, model, failure, round)
            .await
        {
            Ok(agent_result) => {
                self.accumulate_usage(&agent_result.meta);
                self.emit_usage_event(svc);
                let decomposition = agent_result.value;
                if decomposition.subtasks.is_empty() {
                    Ok(Err("fix agent produced no subtasks".into()))
                } else {
                    Ok(Ok(decomposition.subtasks))
                }
            }
            Err(e) => {
                eprintln!("warning: fix subtask design failed: {e}");
                Ok(Err(format!("fix design failed: {e}")))
            }
        }
    }

    pub const fn recovery_budget_check(&self, limits: &LimitsConfig) -> bool {
        self.recovery_rounds < limits.max_recovery_rounds
    }

    /// Assess whether recovery is possible and design recovery subtasks.
    pub async fn assess_and_design_recovery<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
        failure: &str,
        round: u32,
    ) -> Result<RecoveryDecision, OrchestratorError> {
        let ctx = self.to_task_context(tree);
        let strategy = match svc.agent.assess_recovery(&ctx, failure).await {
            Ok(agent_result) => {
                self.accumulate_usage(&agent_result.meta);
                self.emit_usage_event(svc);
                match agent_result.value {
                    Some(s) => s,
                    None => {
                        return Ok(RecoveryDecision::Unrecoverable {
                            reason: failure.to_string(),
                        });
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: recovery assessment failed: {e}");
                return Ok(RecoveryDecision::Unrecoverable {
                    reason: failure.to_string(),
                });
            }
        };

        self.increment_recovery_rounds();

        self.record_to_vault(
            svc,
            "FINDINGS",
            &format!("Recovery round {round}.\nFailure: {failure}\nStrategy: {strategy}"),
        )
        .await;

        let _ = svc.events.send(Event::RecoveryStarted {
            task_id: self.id,
            round,
        });

        let ctx = self.to_task_context(tree);
        let plan = match svc
            .agent
            .design_recovery_subtasks(&ctx, failure, &strategy, round)
            .await
        {
            Ok(agent_result) => {
                self.accumulate_usage(&agent_result.meta);
                self.emit_usage_event(svc);
                agent_result.value
            }
            Err(e) => {
                eprintln!("warning: recovery plan design failed: {e}");
                return Ok(RecoveryDecision::Unrecoverable {
                    reason: format!("recovery design failed: {failure}"),
                });
            }
        };

        if plan.subtasks.is_empty() {
            return Ok(RecoveryDecision::Unrecoverable {
                reason: format!("recovery produced no subtasks: {failure}"),
            });
        }

        let approach = if plan.full_redecomposition {
            "full"
        } else {
            "incremental"
        };
        let _ = svc.events.send(Event::RecoveryPlanSelected {
            task_id: self.id,
            approach: approach.into(),
        });

        Ok(RecoveryDecision::Plan {
            specs: plan.subtasks,
            supersede_pending: plan.full_redecomposition,
        })
    }

    /// Handle checkpoint after a child reports discoveries.
    pub async fn handle_checkpoint<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
        child_discoveries: &[String],
    ) -> Result<ChildResponse, OrchestratorError> {
        let ctx = self.to_task_context(tree);
        let decision = match svc.agent.checkpoint(&ctx, child_discoveries).await {
            Ok(agent_result) => {
                self.accumulate_usage(&agent_result.meta);
                self.emit_usage_event(svc);
                agent_result.value
            }
            Err(e) => {
                eprintln!("warning: checkpoint classification failed: {e}");
                CheckpointDecision::Proceed
            }
        };

        match decision {
            CheckpointDecision::Proceed => Ok(ChildResponse::Continue),
            CheckpointDecision::Adjust { guidance } => {
                let _ = svc
                    .events
                    .send(Event::CheckpointAdjust { task_id: self.id });
                let vault_content = format!(
                    "Checkpoint adjust.\nDiscoveries: {}\nGuidance: {guidance}",
                    child_discoveries.join("; ")
                );
                self.append_checkpoint_guidance(&guidance);
                self.record_to_vault(svc, "FINDINGS", &vault_content).await;
                Ok(ChildResponse::Continue)
            }
            CheckpointDecision::Escalate => {
                let _ = svc
                    .events
                    .send(Event::CheckpointEscalate { task_id: self.id });
                self.set_checkpoint_guidance(None);
                let escalation_reason = format!(
                    "checkpoint escalation: discoveries invalidate current plan. Discoveries: {}",
                    child_discoveries.join("; ")
                );
                if self.is_fix_task {
                    return Ok(ChildResponse::Failed(escalation_reason));
                }
                if !self.recovery_budget_check(&svc.limits) {
                    let max_recovery = svc.limits.max_recovery_rounds;
                    return Ok(ChildResponse::Failed(format!(
                        "recovery rounds exhausted ({max_recovery}): {escalation_reason}"
                    )));
                }
                let round = self.recovery_rounds + 1;
                match self
                    .assess_and_design_recovery(tree, svc, &escalation_reason, round)
                    .await?
                {
                    RecoveryDecision::Unrecoverable { reason } => Ok(ChildResponse::Failed(reason)),
                    RecoveryDecision::Plan {
                        specs,
                        supersede_pending,
                    } => Ok(ChildResponse::NeedRecoverySubtasks {
                        specs,
                        supersede_pending,
                    }),
                }
            }
        }
    }

    /// Branch-specific scope circuit breaker check.
    pub async fn check_branch_scope<A: AgentService>(&self, svc: &Services<A>) -> ScopeCheck {
        let magnitude = match &self.magnitude {
            Some(m) => m.clone(),
            None => return ScopeCheck::WithinBounds,
        };
        let project_root = match &svc.project_root {
            Some(p) => p.clone(),
            None => return ScopeCheck::WithinBounds,
        };
        scope::git_diff_numstat(&project_root)
            .await
            .map_or(ScopeCheck::WithinBounds, |stdout| {
                scope::evaluate_scope(&stdout, &magnitude)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::LimitsConfig;
    use crate::task::TaskId;

    #[test]
    fn fix_budget_check_cases() {
        let cases: &[(u32, Option<TaskId>, Option<Model>)] = &[
            (0, Some(TaskId(99)), Some(Model::Sonnet)),
            (3, None, Some(Model::Opus)),
            (3, Some(TaskId(99)), None),
            (4, None, None),
        ];
        let limits = LimitsConfig::default();
        for &(rounds, ref parent_id, expected) in cases {
            let mut t = Task::new(TaskId(0), *parent_id, "t".into(), vec![], 0);
            t.verification_fix_rounds = rounds;
            let is_root = parent_id.is_none();
            match (t.fix_round_budget_check(&limits), expected) {
                (FixBudgetCheck::WithinBudget { model }, Some(exp)) => {
                    assert_eq!(model, exp, "rounds={rounds} is_root={is_root}");
                }
                (FixBudgetCheck::Exhausted, None) => {}
                (result, _) => panic!("rounds={rounds} is_root={is_root}: unexpected {result:?}"),
            }
        }
    }

    #[test]
    fn recovery_budget_within() {
        let mut t = Task::new(TaskId(0), None, "t".into(), vec![], 0);
        t.recovery_rounds = 1;
        let limits = LimitsConfig::default();
        assert!(t.recovery_budget_check(&limits));
    }

    #[test]
    fn recovery_budget_exhausted() {
        let mut t = Task::new(TaskId(0), None, "t".into(), vec![], 0);
        t.recovery_rounds = 2;
        let limits = LimitsConfig::default();
        assert!(!t.recovery_budget_check(&limits));
    }
}
