// Leaf helpers: verification model, clamp logic.

use crate::task::{Model, Task, TaskPath};

impl Task {
    pub(crate) fn verification_model(&self) -> Model {
        match self.path {
            Some(TaskPath::Leaf) => {
                let impl_model = self.current_model.unwrap_or(Model::Haiku);
                impl_model.clamp(Model::Haiku, Model::Sonnet)
            }
            _ => Model::Sonnet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::LimitsConfig;
    use crate::events::{Event, EventLog};
    use crate::orchestrator::context::TreeContext;
    use crate::task::node_impl::EpicTask;
    use crate::task::{Model, TaskId, TaskOutcome, TaskPath, TaskPhase, TaskRuntime};
    use crate::test_support::{MockAgentService, MockBuilder};
    use std::sync::Arc;

    fn make_runtime(
        mock: MockAgentService,
    ) -> (
        Arc<TaskRuntime<MockAgentService>>,
        Arc<MockAgentService>,
        EventLog,
    ) {
        make_runtime_with_limits(mock, LimitsConfig::default())
    }

    fn make_runtime_with_limits(
        mock: MockAgentService,
        limits: LimitsConfig,
    ) -> (
        Arc<TaskRuntime<MockAgentService>>,
        Arc<MockAgentService>,
        EventLog,
    ) {
        let log = EventLog::new();
        let mock_arc = Arc::new(mock);
        let rt = Arc::new(TaskRuntime {
            agent: Arc::clone(&mock_arc),
            events: log.clone(),
            vault: None,
            limits,
            project_root: None,
        });
        (rt, mock_arc, log)
    }

    fn make_leaf_task(rt: &Arc<TaskRuntime<MockAgentService>>) -> EpicTask<MockAgentService> {
        let mut task = Task::new(
            TaskId(1),
            Some(TaskId(0)),
            "child task".into(),
            vec!["child passes".into()],
            1,
        );
        task.path = Some(TaskPath::Leaf);
        task.current_model = Some(Model::Haiku);
        task.phase = TaskPhase::Executing;
        EpicTask::new(task, Some(Arc::clone(rt)))
    }

    fn empty_tree() -> TreeContext {
        TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: Vec::new(),
            ancestor_goals: Vec::new(),
            completed_siblings: Vec::new(),
            pending_sibling_goals: Vec::new(),
            children: Vec::new(),
            checkpoint_guidance: None,
        }
    }

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

    // -----------------------------------------------------------------------
    // Retry / escalation
    // -----------------------------------------------------------------------

    /// Haiku fails 3x -> escalate to Sonnet -> succeeds.
    #[tokio::test]
    async fn leaf_retry_and_escalation() {
        let mock = MockBuilder::new()
            .leaf_failures(3, "haiku failed")
            .leaf_success()
            .verify_pass()
            .file_review_pass()
            .build();
        let (rt, _mock_arc, _log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(task.task.attempts.len(), 4);
        assert_eq!(task.task.current_model, Some(Model::Sonnet));
    }

    /// All tiers exhausted (9 failures: 3 Haiku + 3 Sonnet + 3 Opus) -> Failed.
    #[tokio::test]
    async fn terminal_failure() {
        let mock = MockBuilder::new()
            .leaf_failures(9, "persistent failure")
            .build();
        let (rt, _mock_arc, _log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert!(matches!(result, TaskOutcome::Failed { .. }));
        assert_eq!(task.task.attempts.len(), 9);
    }

    /// Custom `retry_budget`=1: Haiku fails once -> immediately escalates to Sonnet.
    #[tokio::test]
    async fn custom_retry_budget_escalates_early() {
        let mock = MockBuilder::new()
            .leaf_failed("haiku failed")
            .leaf_success()
            .verify_pass()
            .file_review_pass()
            .build();
        let limits = LimitsConfig {
            retry_budget: 1,
            ..LimitsConfig::default()
        };
        let (rt, _mock_arc, _log) = make_runtime_with_limits(mock, limits);
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(task.task.attempts.len(), 2);
        assert_eq!(task.task.current_model, Some(Model::Sonnet));
    }

    // -----------------------------------------------------------------------
    // Fix loop
    // -----------------------------------------------------------------------

    /// Verification fails -> `fix_leaf` succeeds -> re-verification passes.
    #[tokio::test]
    async fn leaf_fix_passes_on_retry() {
        let mock = MockBuilder::new()
            .leaf_success()
            .verify_fail("test X not passing")
            .fix_leaf_success()
            .verify_pass()
            .file_review_pass()
            .build();
        let (rt, _mock_arc, _log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(task.task.fix_attempts.len(), 1);
        assert!(task.task.fix_attempts[0].succeeded);
    }

    /// Fix loop: 3 failures at starting tier -> escalate -> fix succeeds -> verify passes.
    #[tokio::test]
    async fn leaf_fix_escalates_model() {
        let mock = MockBuilder::new()
            .leaf_success()
            .verify_fail("tests fail")
            .fix_leaf_failures(3, "could not fix")
            .fix_leaf_success()
            .verify_pass()
            .file_review_pass()
            .build();
        let (rt, _mock_arc, log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let task_id = task.task.id;
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(task.task.fix_attempts.len(), 4);
        assert_eq!(task.task.current_model, Some(Model::Sonnet));

        let mut found_escalation = false;
        for event in log.snapshot() {
            if matches!(event, Event::FixModelEscalated { task_id: id, from: Model::Haiku, to: Model::Sonnet } if id == task_id)
            {
                found_escalation = true;
            }
        }
        assert!(found_escalation, "FixModelEscalated event not found");
    }

    /// Fix loop: all tiers exhausted (9 fix failures) -> terminal failure.
    #[tokio::test]
    async fn leaf_fix_terminal_failure() {
        let mock = MockBuilder::new()
            .leaf_success()
            .verify_fail("tests fail")
            .fix_leaf_failures(9, "still broken")
            .build();
        let (rt, _mock_arc, _log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert!(matches!(result, TaskOutcome::Failed { .. }));
        assert_eq!(task.task.fix_attempts.len(), 9);
    }

    /// Fix loop: `verify()` returns Err on first attempt, succeeds on second.
    #[tokio::test]
    async fn leaf_fix_verify_error_retries() {
        let mut mb = MockBuilder::new();
        mb.leaf_success().verify_fail("tests fail");
        mb.fix_leaf_success();
        mb.verify_errors_sequence(TaskId(1), vec![None, Some("transient API error".into())]);
        mb.fix_leaf_success();
        mb.verify_pass().file_review_pass();
        let (rt, _mock_arc, _log) = make_runtime(mb.build());
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(task.task.fix_attempts.len(), 2);
    }

    /// All leaf fix retries across all tiers fail verification -> Failed.
    #[tokio::test]
    async fn leaf_fix_verify_error_exhausts_budget() {
        let mut mb = MockBuilder::new();
        mb.leaf_success().verify_fail("tests fail");
        let mut errors: Vec<Option<String>> = vec![None];
        errors.extend(std::iter::repeat_n(
            Some("persistent verify error".into()),
            9,
        ));
        mb.verify_errors_sequence(TaskId(1), errors);
        for _ in 0..9 {
            mb.fix_leaf_success();
        }
        let (rt, _mock_arc, _log) = make_runtime(mb.build());
        let mut task = make_leaf_task(&rt);
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert!(matches!(result, TaskOutcome::Failed { .. }));
        assert_eq!(task.task.fix_attempts.len(), 9);
    }

    // -----------------------------------------------------------------------
    // File-level review
    // -----------------------------------------------------------------------

    /// Leaf passes file-level review -> completes normally.
    #[tokio::test]
    async fn file_level_review_pass_completes() {
        let mock = MockBuilder::new()
            .leaf_success()
            .verify_pass()
            .file_review_pass()
            .build();
        let (rt, _mock_arc, log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let task_id = task.task.id;
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);

        let mut saw_review_passed = false;
        for event in log.snapshot() {
            if matches!(event, Event::FileLevelReviewCompleted { task_id: id, passed } if id == task_id && passed)
            {
                saw_review_passed = true;
            }
        }
        assert!(
            saw_review_passed,
            "FileLevelReviewCompleted(passed=true) event not found"
        );
    }

    /// Leaf fails file-level review -> enters fix loop -> succeeds.
    #[tokio::test]
    async fn file_level_review_fail_triggers_fix_loop() {
        let mock = MockBuilder::new()
            .leaf_success()
            .verify_pass()
            .file_review_fail("missing error handling")
            .fix_leaf_success()
            .verify_pass()
            .file_review_pass()
            .build();
        let (rt, _mock_arc, log) = make_runtime(mock);
        let mut task = make_leaf_task(&rt);
        let task_id = task.task.id;
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert_eq!(result, TaskOutcome::Success);
        assert_eq!(task.task.fix_attempts.len(), 1);

        let mut review_events: Vec<bool> = Vec::new();
        for event in log.snapshot() {
            if let Event::FileLevelReviewCompleted {
                task_id: id,
                passed,
            } = event
            {
                if id == task_id {
                    review_events.push(passed);
                }
            }
        }
        assert_eq!(
            review_events,
            vec![false, true],
            "expected [failed, passed] review events"
        );
    }

    /// Fix task that fails file-level review -> fails immediately (no fix loop).
    #[tokio::test]
    async fn fix_task_file_review_fail_immediate_failure() {
        let mock = MockBuilder::new()
            .leaf_success()
            .verify_pass()
            .file_review_fail("fix incomplete")
            .build();
        let (rt, _mock_arc, _log) = make_runtime(mock);
        let mut task_inner = Task::new(
            TaskId(1),
            Some(TaskId(0)),
            "child task".into(),
            vec!["child passes".into()],
            1,
        );
        task_inner.path = Some(TaskPath::Leaf);
        task_inner.current_model = Some(Model::Haiku);
        task_inner.phase = TaskPhase::Executing;
        task_inner.is_fix_task = true;
        let mut task = EpicTask::new(task_inner, Some(Arc::clone(&rt)));
        let tree = empty_tree();
        let result = cue::TaskNode::execute_leaf(&mut task, &tree).await;
        assert!(matches!(result, TaskOutcome::Failed { .. }));
        assert_eq!(
            task.task.fix_attempts.len(),
            0,
            "fix task should not enter fix loop on file-level review failure"
        );
    }
}
