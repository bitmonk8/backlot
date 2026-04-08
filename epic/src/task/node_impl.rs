// TaskNode implementation: bridges epic's Task + AgentService to cue's trait contract.
// Wraps the concrete Task with runtime deps so lifecycle methods can call the agent.

use crate::agent::{AgentService, TaskContext};
use crate::events::Event;
use crate::orchestrator::context::TreeContext;
use crate::task::assess::AssessmentResult;
use crate::task::branch::{
    BranchVerifyOutcome, CheckpointDecision, ChildResponse, DecompositionResult, FixBudgetCheck,
    RecoveryDecision, SubtaskSpec,
};
use crate::task::scope::ScopeCheck;
use crate::task::verify::{VerificationOutcome, VerifyOutcome};
use crate::task::{
    Magnitude, Model, RecoveryEligibility, RegistrationInfo, ResumePoint, SessionMeta, Task,
    TaskId, TaskOutcome, TaskPath, TaskPhase, TaskRuntime,
};
use cue::OrchestratorError;
use cue::config::LimitsConfig;
use std::sync::Arc;

/// Combines a serializable `Task` with non-serializable runtime deps.
/// Stored in `EpicStore` and returned via `TaskStore::get/get_mut`.
pub struct EpicTask<A: AgentService> {
    pub task: Task,
    pub runtime: Option<Arc<TaskRuntime<A>>>,
}

impl<A: AgentService> EpicTask<A> {
    pub const fn new(task: Task, runtime: Option<Arc<TaskRuntime<A>>>) -> Self {
        Self { task, runtime }
    }

    fn rt(&self) -> &TaskRuntime<A> {
        self.runtime
            .as_ref()
            .expect("runtime not bound; call bind_runtime before running orchestrator")
    }

    /// Clone the runtime Arc so we can mutate self.task without borrow conflicts.
    fn rt_arc(&self) -> Arc<TaskRuntime<A>> {
        Arc::clone(
            self.runtime
                .as_ref()
                .expect("runtime not bound; call bind_runtime before running orchestrator"),
        )
    }

    fn build_task_context(&self, tree: &TreeContext) -> TaskContext {
        crate::orchestrator::context::tree_to_task_context(tree, &self.task)
    }

    /// Reorganize vault documents (best-effort).
    async fn reorganize_vault(&mut self) {
        let rt = self.rt_arc();
        let Some(ref vault) = rt.vault else {
            return;
        };
        match vault.reorganize().await {
            Ok((report, _warnings, meta)) => {
                let session_meta = crate::agent::session_meta_from_vault(&meta);
                self.task.accumulate_usage(&session_meta);
                self.emit_usage_event();
                rt.events.emit(Event::VaultReorganizeCompleted {
                    merged: report.merged.len(),
                    restructured: report.restructured.len(),
                    deleted: report.deleted.len(),
                });
            }
            Err(e) => {
                eprintln!("warning: vault reorganize failed: {e}");
            }
        }
    }

    /// Record content to vault (best-effort).
    async fn record_to_vault(&mut self, name: &str, content: &str) {
        let rt = self.rt_arc();
        let Some(ref vault) = rt.vault else {
            return;
        };
        let result = match vault.record(name, content, vault::RecordMode::New).await {
            Err(vault::RecordError::VersionConflict(_)) => {
                vault.record(name, content, vault::RecordMode::Append).await
            }
            other => other,
        };
        match result {
            Ok((_refs, _warnings, meta)) => {
                let session_meta = crate::agent::session_meta_from_vault(&meta);
                self.task.accumulate_usage(&session_meta);
                self.emit_usage_event();
                rt.events.emit(Event::VaultRecorded {
                    task_id: self.task.id,
                    document: name.to_string(),
                });
            }
            Err(e) => {
                eprintln!("warning: vault record failed for {name}: {e}");
            }
        }
    }

    /// Emit usage updated event.
    fn emit_usage_event(&self) {
        let rt = self.rt();
        rt.events.emit(Event::UsageUpdated {
            task_id: self.task.id,
            phase_cost_usd: 0.0,
            total_cost_usd: self.task.usage.cost_usd,
        });
    }

    fn emit_escalation(rt: &TaskRuntime<A>, id: TaskId, from: Model, to: Model, is_fix: bool) {
        if is_fix {
            rt.events.emit(Event::FixModelEscalated {
                task_id: id,
                from,
                to,
            });
        } else {
            rt.events.emit(Event::ModelEscalated {
                task_id: id,
                from,
                to,
            });
        }
    }

    /// Map a `VerificationOutcome` to a branch-fail outcome, returning `None` on pass.
    fn branch_fail_outcome(&self, outcome: VerificationOutcome) -> Option<BranchVerifyOutcome> {
        match outcome {
            VerificationOutcome::Pass => None,
            VerificationOutcome::Fail { reason } => Some(if self.task.is_fix_task {
                BranchVerifyOutcome::FailedNoFixLoop { reason }
            } else {
                BranchVerifyOutcome::Failed { reason }
            }),
        }
    }
}

impl<A: AgentService + 'static> cue::TaskNode for EpicTask<A> {
    // --- Read accessors ---

    fn id(&self) -> TaskId {
        self.task.id
    }

    fn parent_id(&self) -> Option<TaskId> {
        self.task.parent_id
    }

    fn goal(&self) -> &str {
        &self.task.goal
    }

    fn depth(&self) -> u32 {
        self.task.depth
    }

    fn phase(&self) -> TaskPhase {
        self.task.phase
    }

    fn subtask_ids(&self) -> &[TaskId] {
        &self.task.subtask_ids
    }

    fn discoveries(&self) -> &[String] {
        &self.task.discoveries
    }

    fn recovery_rounds(&self) -> u32 {
        self.task.recovery_rounds
    }

    // --- Decision methods ---

    fn is_terminal(&self) -> bool {
        self.task.is_terminal()
    }

    fn resume_point(&self) -> ResumePoint {
        self.task.resume_point()
    }

    fn forced_assessment(&self, max_depth: u32) -> Option<AssessmentResult> {
        self.task.forced_assessment(max_depth)
    }

    fn needs_decomposition(&self) -> bool {
        self.task.needs_decomposition()
    }

    fn decompose_model(&self) -> Model {
        self.task.decompose_model()
    }

    fn registration_info(&self) -> RegistrationInfo {
        self.task.registration_info()
    }

    // --- Mutations ---

    fn set_phase(&mut self, phase: TaskPhase) {
        self.task.phase = phase;
    }

    fn set_assessment(&mut self, path: TaskPath, model: Model, magnitude: Option<Magnitude>) {
        self.task.set_assessment(path, model, magnitude);
    }

    fn set_decomposition_rationale(&mut self, rationale: String) {
        self.task.set_decomposition_rationale(rationale);
    }

    fn set_subtask_ids(&mut self, ids: &[TaskId], append: bool) {
        if append {
            self.task.subtask_ids.extend_from_slice(ids);
        } else {
            self.task.subtask_ids = ids.to_vec();
        }
    }

    fn increment_fix_rounds(&mut self) -> u32 {
        self.task.increment_fix_rounds()
    }

    fn accumulate_usage(&mut self, meta: &SessionMeta) -> f64 {
        self.task.accumulate_usage(meta);
        self.task.usage.cost_usd
    }

    // --- Lifecycle (async) ---

    async fn execute_leaf(&mut self, ctx: &TreeContext) -> TaskOutcome {
        // Delegate to existing leaf execution logic which uses the runtime.
        self.execute_leaf_impl(ctx).await
    }

    async fn verify_branch(
        &mut self,
        ctx: &TreeContext,
    ) -> Result<BranchVerifyOutcome, OrchestratorError> {
        // Reorganize vault before branch verification (best-effort).
        self.reorganize_vault().await;

        let rt = self.rt_arc();
        let verify_model = self.task.verification_model();
        let task_ctx = self.build_task_context(ctx);

        // Phase 1: Correctness review.
        let correctness = rt
            .agent
            .verify_branch_correctness(&task_ctx, verify_model)
            .await?;
        self.task.accumulate_usage(&correctness.meta);
        self.emit_usage_event();

        if let Some(outcome) = self.branch_fail_outcome(correctness.value.outcome) {
            return Ok(outcome);
        }

        // Phase 2: Completeness review.
        let completeness = rt
            .agent
            .verify_branch_completeness(&task_ctx, verify_model)
            .await?;
        self.task.accumulate_usage(&completeness.meta);
        self.emit_usage_event();

        if let Some(outcome) = self.branch_fail_outcome(completeness.value.outcome) {
            return Ok(outcome);
        }

        // Phase 3: Aggregate simplification review.
        let simplification = rt
            .agent
            .verify_branch_simplification(&task_ctx, verify_model)
            .await?;
        self.task.accumulate_usage(&simplification.meta);
        self.emit_usage_event();

        if let Some(outcome) = self.branch_fail_outcome(simplification.value.outcome) {
            return Ok(outcome);
        }

        Ok(BranchVerifyOutcome::Passed)
    }

    fn fix_round_budget_check(&self, limits: &LimitsConfig) -> FixBudgetCheck {
        self.task.fix_round_budget_check(limits)
    }

    async fn check_branch_scope(&self) -> ScopeCheck {
        self.check_scope_impl().await
    }

    async fn design_fix(
        &mut self,
        ctx: &TreeContext,
        failure_reason: &str,
        round: u32,
        model: Model,
    ) -> Result<Result<Vec<SubtaskSpec>, String>, OrchestratorError> {
        let rt = self.rt_arc();
        let task_ctx = self.build_task_context(ctx);
        match rt
            .agent
            .design_fix_subtasks(&task_ctx, model, failure_reason, round)
            .await
        {
            Ok(agent_result) => {
                self.task.accumulate_usage(&agent_result.meta);
                self.emit_usage_event();
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

    async fn handle_checkpoint(
        &mut self,
        ctx: &TreeContext,
        child_discoveries: &[String],
    ) -> Result<ChildResponse, OrchestratorError> {
        let rt = self.rt_arc();
        let task_ctx = self.build_task_context(ctx);
        let decision = match rt.agent.checkpoint(&task_ctx, child_discoveries).await {
            Ok(agent_result) => {
                self.task.accumulate_usage(&agent_result.meta);
                self.emit_usage_event();
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
                rt.events.emit(Event::CheckpointAdjust {
                    task_id: self.task.id,
                });
                let vault_content = format!(
                    "Checkpoint adjust.\nDiscoveries: {}\nGuidance: {guidance}",
                    child_discoveries.join("; ")
                );
                self.task.append_checkpoint_guidance(&guidance);
                self.record_to_vault("FINDINGS", &vault_content).await;
                Ok(ChildResponse::Continue)
            }
            CheckpointDecision::Escalate => {
                rt.events.emit(Event::CheckpointEscalate {
                    task_id: self.task.id,
                });
                self.task.set_checkpoint_guidance(None);
                let escalation_reason = format!(
                    "checkpoint escalation: discoveries invalidate current plan. Discoveries: {}",
                    child_discoveries.join("; ")
                );
                if self.task.is_fix_task {
                    return Ok(ChildResponse::Failed(escalation_reason));
                }
                if !self.task.recovery_budget_check(&rt.limits) {
                    let max_recovery = rt.limits.max_recovery_rounds;
                    return Ok(ChildResponse::Failed(format!(
                        "recovery rounds exhausted ({max_recovery}): {escalation_reason}"
                    )));
                }
                let round = self.task.recovery_rounds + 1;
                match self
                    .assess_and_design_recovery_impl(ctx, &escalation_reason, round)
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

    fn can_attempt_recovery(&self, limits: &LimitsConfig) -> RecoveryEligibility {
        self.task.can_attempt_recovery(limits)
    }

    async fn assess_and_design_recovery(
        &mut self,
        ctx: &TreeContext,
        failure_reason: &str,
        round: u32,
    ) -> Result<RecoveryDecision, OrchestratorError> {
        self.assess_and_design_recovery_impl(ctx, failure_reason, round)
            .await
    }

    async fn assess(&mut self, ctx: &TreeContext) -> Result<AssessmentResult, OrchestratorError> {
        let rt = self.rt_arc();
        let task_ctx = self.build_task_context(ctx);
        let agent_result = rt.agent.assess(&task_ctx).await?;
        self.task.accumulate_usage(&agent_result.meta);
        self.emit_usage_event();
        Ok(agent_result.value)
    }

    async fn decompose(
        &mut self,
        ctx: &TreeContext,
        model: Model,
    ) -> Result<DecompositionResult, OrchestratorError> {
        let rt = self.rt_arc();
        let task_ctx = self.build_task_context(ctx);
        let agent_result = rt.agent.design_and_decompose(&task_ctx, model).await?;
        self.task.accumulate_usage(&agent_result.meta);
        self.emit_usage_event();
        Ok(agent_result.value)
    }
}

// --- Private lifecycle implementations ---

impl<A: AgentService + 'static> EpicTask<A> {
    /// Full leaf lifecycle delegating to the existing retry/escalation/fix logic.
    async fn execute_leaf_impl(&mut self, tree: &TreeContext) -> TaskOutcome {
        // Resume: if task was mid-verification, go straight to verify+fix.
        if self.task.phase == TaskPhase::Verifying {
            return self.leaf_finalize(tree).await;
        }

        match self.leaf_retry_loop(tree, RetryMode::Execute).await {
            Ok(exec_outcome) => {
                if exec_outcome == TaskOutcome::Success {
                    self.leaf_finalize(tree).await
                } else {
                    exec_outcome
                }
            }
            Err(e) => TaskOutcome::Failed {
                reason: format!("agent error: {e}"),
            },
        }
    }

    async fn leaf_finalize(&mut self, tree: &TreeContext) -> TaskOutcome {
        let rt = self.rt_arc();
        let verify_model = self.task.verification_model();
        let ctx = self.build_task_context(tree);
        let agent_result = match rt.agent.verify(&ctx, verify_model).await {
            Ok(r) => r,
            Err(e) => {
                return TaskOutcome::Failed {
                    reason: format!("__agent_error__: {e}"),
                };
            }
        };
        self.task.accumulate_usage(&agent_result.meta);
        self.emit_usage_event();

        match agent_result.value.outcome {
            VerificationOutcome::Pass => {
                if let Some(fail_reason) = self.try_file_level_review(tree).await {
                    if self.task.is_fix_task {
                        TaskOutcome::Failed {
                            reason: fail_reason,
                        }
                    } else {
                        self.leaf_fix_loop(tree, &fail_reason).await
                    }
                } else {
                    // File-level review passed; now check for simplification opportunities.
                    if let Some(fail_reason) = self.try_leaf_simplification_review(tree).await {
                        if self.task.is_fix_task {
                            TaskOutcome::Failed {
                                reason: fail_reason,
                            }
                        } else {
                            self.leaf_fix_loop(tree, &fail_reason).await
                        }
                    } else {
                        TaskOutcome::Success
                    }
                }
            }
            VerificationOutcome::Fail { reason } => {
                self.record_to_vault("VERIFICATION_FAILURE", &reason).await;
                if self.task.is_fix_task {
                    TaskOutcome::Failed { reason }
                } else {
                    self.leaf_fix_loop(tree, &reason).await
                }
            }
        }
    }

    async fn leaf_fix_loop(&mut self, tree: &TreeContext, initial_failure: &str) -> TaskOutcome {
        match self
            .leaf_retry_loop(
                tree,
                RetryMode::Fix {
                    initial_failure: initial_failure.to_owned(),
                },
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => TaskOutcome::Failed {
                reason: format!("agent error: {e}"),
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn leaf_retry_loop(
        &mut self,
        tree: &TreeContext,
        mode: RetryMode,
    ) -> anyhow::Result<TaskOutcome> {
        let is_fix = matches!(mode, RetryMode::Fix { .. });
        let mut failure_reason = match &mode {
            RetryMode::Fix { initial_failure } => Some(initial_failure.clone()),
            RetryMode::Execute => None,
        };

        let rt = self.rt_arc();
        let retry_budget = rt.limits.retry_budget;

        let mut current_model = self.task.current_model.unwrap_or(Model::Haiku);
        let mut retries_at_tier: u32 = self.task.trailing_attempts_at_tier(current_model, is_fix);

        while retries_at_tier >= retry_budget {
            if let Some(next_model) = current_model.escalate() {
                Self::emit_escalation(&rt, self.task.id, current_model, next_model, is_fix);
                self.task.set_model(next_model);
                current_model = next_model;
                retries_at_tier = 0;
            } else if is_fix {
                return Ok(TaskOutcome::Failed {
                    reason: failure_reason.unwrap_or_else(|| "all tiers exhausted".into()),
                });
            } else {
                let last_error = self
                    .task
                    .attempts
                    .last()
                    .and_then(|a| a.error.clone())
                    .unwrap_or_else(|| "all tiers exhausted".into());
                return Ok(TaskOutcome::Failed { reason: last_error });
            }
        }

        loop {
            if is_fix {
                match self.check_scope_impl().await {
                    ScopeCheck::WithinBounds => {}
                    ScopeCheck::Exceeded {
                        metric,
                        actual,
                        limit,
                    } => {
                        return Ok(TaskOutcome::Failed {
                            reason: format!(
                                "SCOPE_EXCEEDED: {metric} actual={actual} limit={limit}"
                            ),
                        });
                    }
                }
            }

            let ctx = self.build_task_context(tree);
            let agent_result = if is_fix {
                let reason = failure_reason.as_deref().unwrap_or("unknown failure");
                #[allow(clippy::cast_possible_truncation)]
                let attempt_number = self.task.fix_attempts.len() as u32 + 1;
                rt.events.emit(Event::FixAttempt {
                    task_id: self.task.id,
                    attempt: attempt_number,
                    model: current_model,
                });
                rt.agent
                    .fix_leaf(&ctx, current_model, reason, attempt_number)
                    .await?
            } else {
                rt.agent.execute_leaf(&ctx, current_model).await?
            };
            self.task.accumulate_usage(&agent_result.meta);
            self.emit_usage_event();

            let cue::LeafResult {
                outcome,
                discoveries,
            } = agent_result.value;

            let attempt = cue::Attempt {
                model: current_model,
                succeeded: outcome == TaskOutcome::Success,
                error: match &outcome {
                    TaskOutcome::Success => None,
                    TaskOutcome::Failed { reason } => Some(reason.clone()),
                },
            };
            self.task.record_attempt(attempt, is_fix);
            if !discoveries.is_empty() {
                let content = discoveries.join("\n");
                let count = self.task.record_discoveries(discoveries);
                rt.events.emit(Event::DiscoveriesRecorded {
                    task_id: self.task.id,
                    count,
                });
                self.record_to_vault("FINDINGS", &content).await;
            }

            if outcome == TaskOutcome::Success {
                if is_fix {
                    match self.try_verify(tree).await {
                        VerifyOutcome::Passed => return Ok(TaskOutcome::Success),
                        VerifyOutcome::Failed(reason) => failure_reason = Some(reason),
                    }
                } else {
                    return Ok(outcome);
                }
            } else if is_fix {
                if let TaskOutcome::Failed { reason } = &outcome {
                    failure_reason = Some(reason.clone());
                }
            }

            retries_at_tier += 1;

            if retries_at_tier < rt.limits.retry_budget {
                if !is_fix {
                    rt.events.emit(Event::RetryAttempt {
                        task_id: self.task.id,
                        attempt: retries_at_tier,
                        model: current_model,
                    });
                }
                continue;
            }

            if let Some(next_model) = current_model.escalate() {
                Self::emit_escalation(&rt, self.task.id, current_model, next_model, is_fix);
                self.task.set_model(next_model);
                current_model = next_model;
                retries_at_tier = 0;
                continue;
            }

            if is_fix {
                return Ok(TaskOutcome::Failed {
                    reason: failure_reason.unwrap_or_else(|| "all tiers exhausted".into()),
                });
            }
            return Ok(outcome);
        }
    }

    async fn try_verify(&mut self, tree: &TreeContext) -> VerifyOutcome {
        let rt = self.rt_arc();
        let verify_model = self.task.verification_model();
        let ctx = self.build_task_context(tree);
        match rt.agent.verify(&ctx, verify_model).await {
            Ok(agent_result) => {
                self.task.accumulate_usage(&agent_result.meta);
                self.emit_usage_event();
                match agent_result.value.outcome {
                    VerificationOutcome::Pass => {
                        if let Some(reason) = self.try_file_level_review(tree).await {
                            VerifyOutcome::Failed(reason)
                        } else if let Some(reason) = self.try_leaf_simplification_review(tree).await
                        {
                            VerifyOutcome::Failed(reason)
                        } else {
                            VerifyOutcome::Passed
                        }
                    }
                    VerificationOutcome::Fail { reason } => {
                        self.record_to_vault("VERIFICATION_FAILURE", &reason).await;
                        VerifyOutcome::Failed(reason)
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: verify failed: {e}");
                VerifyOutcome::Failed(format!("verification error: {e}"))
            }
        }
    }

    async fn try_file_level_review(&mut self, tree: &TreeContext) -> Option<String> {
        let rt = self.rt_arc();
        let review_model = self.task.verification_model();
        let ctx = self.build_task_context(tree);
        let review_result = match rt.agent.file_level_review(&ctx, review_model).await {
            Ok(r) => r,
            Err(e) => {
                return Some(format!("file-level review error: {e}"));
            }
        };
        self.task.accumulate_usage(&review_result.meta);
        self.emit_usage_event();

        let passed = review_result.value.outcome == VerificationOutcome::Pass;
        rt.events.emit(Event::FileLevelReviewCompleted {
            task_id: self.task.id,
            passed,
        });

        match review_result.value.outcome {
            VerificationOutcome::Pass => None,
            VerificationOutcome::Fail { reason } => Some(reason),
        }
    }

    async fn try_leaf_simplification_review(&mut self, tree: &TreeContext) -> Option<String> {
        let rt = self.rt_arc();
        let review_model = self.task.verification_model();
        let ctx = self.build_task_context(tree);
        let review_result = match rt
            .agent
            .leaf_simplification_review(&ctx, review_model)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Some(format!("leaf simplification review error: {e}"));
            }
        };
        self.task.accumulate_usage(&review_result.meta);
        self.emit_usage_event();

        let passed = review_result.value.outcome == VerificationOutcome::Pass;
        rt.events.emit(Event::LeafSimplificationReviewCompleted {
            task_id: self.task.id,
            passed,
        });

        match review_result.value.outcome {
            VerificationOutcome::Pass => None,
            VerificationOutcome::Fail { reason } => Some(reason),
        }
    }

    async fn check_scope_impl(&self) -> ScopeCheck {
        let rt = self.rt();
        let magnitude = match &self.task.magnitude {
            Some(m) => m.clone(),
            None => return ScopeCheck::WithinBounds,
        };
        let project_root = match &rt.project_root {
            Some(p) => p.clone(),
            None => return ScopeCheck::WithinBounds,
        };
        crate::task::scope::git_diff_numstat(&project_root)
            .await
            .map_or(ScopeCheck::WithinBounds, |stdout| {
                crate::task::scope::evaluate_scope(&stdout, &magnitude)
            })
    }

    async fn assess_and_design_recovery_impl(
        &mut self,
        ctx: &TreeContext,
        failure: &str,
        round: u32,
    ) -> Result<RecoveryDecision, OrchestratorError> {
        let rt = self.rt_arc();
        let task_ctx = self.build_task_context(ctx);
        let strategy = match rt.agent.assess_recovery(&task_ctx, failure).await {
            Ok(agent_result) => {
                self.task.accumulate_usage(&agent_result.meta);
                self.emit_usage_event();
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

        self.task.increment_recovery_rounds();

        self.record_to_vault(
            "FINDINGS",
            &format!("Recovery round {round}.\nFailure: {failure}\nStrategy: {strategy}"),
        )
        .await;

        rt.events.emit(Event::RecoveryStarted {
            task_id: self.task.id,
            round,
        });

        let task_ctx = self.build_task_context(ctx);
        let plan = match rt
            .agent
            .design_recovery_subtasks(&task_ctx, failure, &strategy, round)
            .await
        {
            Ok(agent_result) => {
                self.task.accumulate_usage(&agent_result.meta);
                self.emit_usage_event();
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
        rt.events.emit(Event::RecoveryPlanSelected {
            task_id: self.task.id,
            approach: approach.into(),
        });

        Ok(RecoveryDecision::Plan {
            specs: plan.subtasks,
            supersede_pending: plan.full_redecomposition,
        })
    }
}

enum RetryMode {
    Execute,
    Fix { initial_failure: String },
}
