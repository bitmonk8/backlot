// Branch execution: budget checks, recovery budget checks.
// Decision logic lives here; cross-task coordination stays in the orchestrator.
// Lifecycle methods that need an agent are on EpicTask in node_impl.rs.

use crate::config::project::LimitsConfig;
use crate::task::{Model, Task};

// Re-export orchestration-protocol types from cue.
pub use cue::{
    BranchVerifyOutcome, CheckpointDecision, ChildResponse, DecompositionResult, FixBudgetCheck,
    RecoveryDecision, SubtaskSpec,
};

impl Task {
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

    pub const fn recovery_budget_check(&self, limits: &LimitsConfig) -> bool {
        self.recovery_rounds < limits.max_recovery_rounds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
