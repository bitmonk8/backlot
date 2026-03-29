// Leaf execution: implement -> verify -> fix loop -> return outcome.
// Handles retry/escalation (Haiku->Sonnet->Opus), scope circuit breaker,
// file-level review, verification gates.

use crate::agent::{AgentService, SessionMeta};
use crate::events::Event;
use crate::orchestrator::context::TreeContext;
use crate::orchestrator::services::Services;
use crate::task::scope::{self, ScopeCheck};
use crate::task::verify::{VerificationOutcome, VerifyOutcome};
use crate::task::{Attempt, Model, Task, TaskOutcome, TaskPath};

/// Distinguishes first-execution from fix-loop retry behavior.
enum RetryMode {
    Execute,
    Fix { initial_failure: String },
}

impl Task {
    /// Full leaf lifecycle: execute -> verify -> fix loop -> return outcome.
    /// Handles resume from mid-execution or mid-verification.
    pub async fn execute_leaf<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> TaskOutcome {
        // Resume: if task was mid-verification, go straight to verify+fix.
        // The orchestrator sets phase to Verifying before calling into verify;
        // if we crashed there, skip re-execution.
        if self.phase == crate::task::TaskPhase::Verifying {
            return self.leaf_finalize(tree, svc).await;
        }

        match self.leaf_retry_loop(tree, svc, RetryMode::Execute).await {
            Ok(exec_outcome) => {
                if exec_outcome == TaskOutcome::Success {
                    self.leaf_finalize(tree, svc).await
                } else {
                    exec_outcome
                }
            }
            Err(e) => TaskOutcome::Failed {
                reason: format!("agent error: {e}"),
            },
        }
    }

    /// Post-execution verification + file-level review + fix loop entry.
    /// Returns `Err` for agent-level failures (propagated to caller as infrastructure error).
    async fn leaf_finalize<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> TaskOutcome {
        let verify_model = self.verification_model();
        let ctx = tree.to_task_context(self);
        // Propagate agent errors as Failed with special prefix so the
        // orchestrator can distinguish infrastructure errors.
        let agent_result = match svc.agent.verify(&ctx, verify_model).await {
            Ok(r) => r,
            Err(e) => {
                return TaskOutcome::Failed {
                    reason: format!("__agent_error__: {e}"),
                };
            }
        };
        self.accumulate_usage(&agent_result.meta);
        self.emit_usage_event(svc);

        match agent_result.value.outcome {
            VerificationOutcome::Pass => {
                if let Some(fail_reason) = self.try_file_level_review(tree, svc).await {
                    if self.is_fix_task {
                        TaskOutcome::Failed {
                            reason: fail_reason,
                        }
                    } else {
                        self.leaf_fix_loop(tree, svc, &fail_reason).await
                    }
                } else {
                    TaskOutcome::Success
                }
            }
            VerificationOutcome::Fail { reason } => {
                self.record_to_vault(svc, "VERIFICATION_FAILURE", &reason)
                    .await;
                if self.is_fix_task {
                    TaskOutcome::Failed { reason }
                } else {
                    self.leaf_fix_loop(tree, svc, &reason).await
                }
            }
        }
    }

    /// Fix loop after initial verification failure.
    async fn leaf_fix_loop<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
        initial_failure: &str,
    ) -> TaskOutcome {
        match self
            .leaf_retry_loop(
                tree,
                svc,
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

    /// Shared retry-with-escalation loop for both first execution and fix loops.
    #[allow(clippy::too_many_lines)]
    async fn leaf_retry_loop<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
        mode: RetryMode,
    ) -> anyhow::Result<TaskOutcome> {
        let is_fix = matches!(mode, RetryMode::Fix { .. });
        let mut failure_reason = match &mode {
            RetryMode::Fix { initial_failure } => Some(initial_failure.clone()),
            RetryMode::Execute => None,
        };

        let mut current_model = self.current_model.unwrap_or(Model::Haiku);
        let mut retries_at_tier: u32 = self.trailing_attempts_at_tier(current_model, is_fix);

        // Drain any stale tier exhaustion from a crash before escalation.
        while retries_at_tier >= svc.limits.retry_budget {
            if let Some(next_model) = current_model.escalate() {
                Self::emit_escalation(svc, self.id, current_model, next_model, is_fix);
                self.set_model(next_model);
                current_model = next_model;
                retries_at_tier = 0;
            } else if is_fix {
                return Ok(TaskOutcome::Failed {
                    reason: failure_reason.unwrap_or_else(|| "all tiers exhausted".into()),
                });
            } else {
                let last_error = self
                    .attempts
                    .last()
                    .and_then(|a| a.error.clone())
                    .unwrap_or_else(|| "all tiers exhausted".into());
                return Ok(TaskOutcome::Failed { reason: last_error });
            }
        }

        loop {
            // Scope circuit breaker (fix mode only).
            if is_fix {
                match self.check_scope(svc).await {
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

            // Agent call.
            let ctx = tree.to_task_context(self);
            let agent_result = if is_fix {
                let reason = failure_reason.as_deref().unwrap_or("unknown failure");
                #[allow(clippy::cast_possible_truncation)]
                let attempt_number = self.fix_attempts.len() as u32 + 1;
                let _ = svc.events.send(Event::FixAttempt {
                    task_id: self.id,
                    attempt: attempt_number,
                    model: current_model,
                });
                svc.agent
                    .fix_leaf(&ctx, current_model, reason, attempt_number)
                    .await?
            } else {
                svc.agent.execute_leaf(&ctx, current_model).await?
            };
            self.accumulate_usage(&agent_result.meta);
            self.emit_usage_event(svc);

            let crate::task::LeafResult {
                outcome,
                discoveries,
            } = agent_result.value;

            // Record attempt and discoveries.
            let attempt = Attempt {
                model: current_model,
                succeeded: outcome == TaskOutcome::Success,
                error: match &outcome {
                    TaskOutcome::Success => None,
                    TaskOutcome::Failed { reason } => Some(reason.clone()),
                },
            };
            self.record_attempt(attempt, is_fix);
            if !discoveries.is_empty() {
                let content = discoveries.join("\n");
                let count = self.record_discoveries(discoveries);
                let _ = svc.events.send(Event::DiscoveriesRecorded {
                    task_id: self.id,
                    count,
                });
                self.record_to_vault(svc, "FINDINGS", &content).await;
            }

            // Handle success.
            if outcome == TaskOutcome::Success {
                if is_fix {
                    match self.try_verify(tree, svc).await {
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

            if retries_at_tier < svc.limits.retry_budget {
                if !is_fix {
                    let _ = svc.events.send(Event::RetryAttempt {
                        task_id: self.id,
                        attempt: retries_at_tier,
                        model: current_model,
                    });
                }
                continue;
            }

            if let Some(next_model) = current_model.escalate() {
                Self::emit_escalation(svc, self.id, current_model, next_model, is_fix);
                self.set_model(next_model);
                current_model = next_model;
                retries_at_tier = 0;
                continue;
            }

            // All tiers exhausted.
            if is_fix {
                return Ok(TaskOutcome::Failed {
                    reason: failure_reason.unwrap_or_else(|| "all tiers exhausted".into()),
                });
            }
            return Ok(outcome);
        }
    }

    /// Verify and file-level review (used in fix loop after successful fix).
    async fn try_verify<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> VerifyOutcome {
        let verify_model = self.verification_model();
        let ctx = tree.to_task_context(self);
        match svc.agent.verify(&ctx, verify_model).await {
            Ok(agent_result) => {
                self.accumulate_usage(&agent_result.meta);
                self.emit_usage_event(svc);
                match agent_result.value.outcome {
                    VerificationOutcome::Pass => self
                        .try_file_level_review(tree, svc)
                        .await
                        .map_or(VerifyOutcome::Passed, VerifyOutcome::Failed),
                    VerificationOutcome::Fail { reason } => {
                        self.record_to_vault(svc, "VERIFICATION_FAILURE", &reason)
                            .await;
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

    /// File-level review for leaf tasks after verification passes.
    async fn try_file_level_review<A: AgentService>(
        &mut self,
        tree: &TreeContext,
        svc: &Services<A>,
    ) -> Option<String> {
        let review_model = self.verification_model();
        let ctx = tree.to_task_context(self);
        let review_result = match svc.agent.file_level_review(&ctx, review_model).await {
            Ok(r) => r,
            Err(e) => {
                return Some(format!("file-level review error: {e}"));
            }
        };
        self.accumulate_usage(&review_result.meta);
        self.emit_usage_event(svc);

        let passed = review_result.value.outcome == VerificationOutcome::Pass;
        let _ = svc.events.send(Event::FileLevelReviewCompleted {
            task_id: self.id,
            passed,
        });

        match review_result.value.outcome {
            VerificationOutcome::Pass => None,
            VerificationOutcome::Fail { reason } => Some(reason),
        }
    }

    pub(crate) fn verification_model(&self) -> Model {
        match self.path {
            Some(TaskPath::Leaf) => {
                let impl_model = self.current_model.unwrap_or(Model::Haiku);
                impl_model.clamp(Model::Haiku, Model::Sonnet)
            }
            _ => Model::Sonnet,
        }
    }

    async fn check_scope<A: AgentService>(&self, svc: &Services<A>) -> ScopeCheck {
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

    /// Record content to vault (best-effort).
    pub(crate) async fn record_to_vault<A: AgentService>(
        &mut self,
        svc: &Services<A>,
        name: &str,
        content: &str,
    ) {
        let Some(ref vault) = svc.vault else {
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
                let session_meta = SessionMeta::from_vault(&meta);
                self.accumulate_usage(&session_meta);
                self.emit_usage_event(svc);
                let _ = svc.events.send(Event::VaultRecorded {
                    task_id: self.id,
                    document: name.to_string(),
                });
            }
            Err(e) => {
                eprintln!("warning: vault record failed for {name}: {e}");
            }
        }
    }

    /// Emit usage updated event based on current accumulated cost.
    pub(crate) fn emit_usage_event<A: AgentService>(&self, svc: &Services<A>) {
        let _ = svc.events.send(Event::UsageUpdated {
            task_id: self.id,
            phase_cost_usd: 0.0, // individual phase cost not tracked at task level
            total_cost_usd: self.usage.cost_usd,
        });
    }

    fn emit_escalation<A: AgentService>(
        svc: &Services<A>,
        id: crate::task::TaskId,
        from: Model,
        to: Model,
        is_fix: bool,
    ) {
        if is_fix {
            let _ = svc.events.send(Event::FixModelEscalated {
                task_id: id,
                from,
                to,
            });
        } else {
            let _ = svc.events.send(Event::ModelEscalated {
                task_id: id,
                from,
                to,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskId, TaskPath};

    #[test]
    fn verification_model_cases() {
        let cases = [
            (TaskPath::Leaf, Model::Haiku, Model::Haiku),
            (TaskPath::Leaf, Model::Sonnet, Model::Sonnet),
            (TaskPath::Leaf, Model::Opus, Model::Sonnet), // capped
            (TaskPath::Branch, Model::Haiku, Model::Sonnet), // branch always Sonnet
        ];
        for (path, current, expected) in cases {
            let mut t = Task::new(TaskId(0), None, "t".into(), vec![], 0);
            let label = format!("path={path:?} model={current:?}");
            t.path = Some(path);
            t.current_model = Some(current);
            assert_eq!(t.verification_model(), expected, "{label}");
        }
    }
}
