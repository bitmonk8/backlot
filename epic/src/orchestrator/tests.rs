use super::*;
use crate::events::{self, EventReceiver};
use crate::task::branch::{DecompositionResult, SubtaskSpec};
use crate::task::{Attempt, MagnitudeEstimate, RecoveryPlan, TaskPath};
use crate::test_support::{MockAgentService, MockBuilder};

// Root gets TaskId(0); subtasks get sequential IDs (TaskId(1), TaskId(2), ...) in creation order.
fn make_orchestrator(
    mock: MockAgentService,
) -> (
    Orchestrator<MockAgentService>,
    EpicState,
    TaskId,
    EventReceiver,
) {
    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let root = Task::new(
        root_id,
        None,
        "root goal".into(),
        vec!["root passes".into()],
        0,
    );
    state.insert(root);
    let (tx, rx) = events::event_channel();
    let orchestrator = Orchestrator::new(mock, tx);
    (orchestrator, state, root_id, rx)
}

/// Root(branch) -> one child(leaf) -> success -> verification pass -> Completed.
#[tokio::test]
async fn single_leaf() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();
    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.phase, TaskPhase::Completed);
    assert_eq!(root.path, Some(TaskPath::Branch));

    let child_id = root.subtask_ids[0];
    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.path, Some(TaskPath::Leaf));
}

/// Root decomposes into 2 -> both succeed -> root Completed.
#[tokio::test]
async fn two_children() {
    let mut mb = MockBuilder::new();
    mb.decompose(DecompositionResult {
        subtasks: vec![
            SubtaskSpec {
                goal: "child A".into(),
                verification_criteria: vec!["A passes".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            },
            SubtaskSpec {
                goal: "child B".into(),
                verification_criteria: vec!["B passes".into()],
                magnitude_estimate: MagnitudeEstimate::Medium,
            },
        ],
        rationale: "two subtasks".into(),
    });
    for _ in 0..2 {
        mb.assess_leaf().leaf_success();
    }
    mb.verify_passes(3).file_review_passes(3);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);
    assert_eq!(state.get(root_id).unwrap().subtask_ids.len(), 2);
}

/// State is checkpointed to disk during execution.
#[tokio::test]
async fn checkpoint_saves_state() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let dir = std::env::temp_dir().join("epic_test_checkpoint");
    std::fs::create_dir_all(&dir).unwrap();
    let state_path = dir.join("state.json");
    let _ = std::fs::remove_file(&state_path);

    let (mut orch, mut state, root_id, _rx) = make_orchestrator(mock);
    state.set_root_id(root_id);
    orch.services.state_path = Some(state_path.clone());

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);
    assert!(state_path.exists(), "state.json should exist after run");

    let loaded = EpicState::load(&state_path).unwrap();
    assert_eq!(loaded.root_id(), Some(root_id));
    let loaded_root = loaded.get(root_id).unwrap();
    assert_eq!(loaded_root.phase, TaskPhase::Completed);
    assert!(!loaded_root.subtask_ids.is_empty());

    std::fs::remove_dir_all(&dir).ok();
}

/// Resume: completed child is NOT re-executed; pending child runs normally.
#[tokio::test]
async fn resume_skips_completed_child() {
    let mock = MockBuilder::new()
        .assess_leaf()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let completed_child_id = state.next_task_id();
    let pending_child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![completed_child_id, pending_child_id];

    let mut completed_child = Task::new(
        completed_child_id,
        Some(root_id),
        "done".into(),
        vec!["done".into()],
        1,
    );
    completed_child.phase = TaskPhase::Completed;

    let pending_child = Task::new(
        pending_child_id,
        Some(root_id),
        "todo".into(),
        vec!["todo".into()],
        1,
    );

    state.insert(root);
    state.insert(completed_child);
    state.insert(pending_child);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    assert_eq!(
        state.get(completed_child_id).unwrap().phase,
        TaskPhase::Completed
    );
    assert_eq!(
        state.get(pending_child_id).unwrap().phase,
        TaskPhase::Completed
    );
}

/// Resume: existing `subtask_ids` on root skips decomposition.
#[tokio::test]
async fn resume_skips_decomposition_when_subtasks_exist() {
    let mock = MockBuilder::new()
        .assess_leaf()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
    root.subtask_ids = vec![child_id];

    let child = Task::new(
        child_id,
        Some(root_id),
        "existing child".into(),
        vec!["child passes".into()],
        1,
    );

    state.insert(root);
    state.insert(child);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.subtask_ids, vec![child_id]);
    assert_eq!(root.phase, TaskPhase::Completed);
}

/// Resume: mid-execution Branch is NOT re-assessed; uses existing path and children.
#[tokio::test]
async fn resume_mid_execution_branch_not_reassessed() {
    let mock = MockBuilder::new()
        .assess_leaf()
        .leaf_success()
        .verify_passes(3)
        .file_review_passes(3)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let mid_id = state.next_task_id();
    let grandchild_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![mid_id];

    let mut mid = Task::new(
        mid_id,
        Some(root_id),
        "mid".into(),
        vec!["mid passes".into()],
        1,
    );
    mid.path = Some(TaskPath::Branch);
    mid.current_model = Some(Model::Sonnet);
    mid.phase = TaskPhase::Executing;
    mid.subtask_ids = vec![grandchild_id];

    let grandchild = Task::new(
        grandchild_id,
        Some(mid_id),
        "grandchild".into(),
        vec!["gc passes".into()],
        2,
    );

    state.insert(root);
    state.insert(mid);
    state.insert(grandchild);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let mid = state.get(mid_id).unwrap();
    assert_eq!(mid.path, Some(TaskPath::Branch));
    assert_eq!(mid.subtask_ids, vec![grandchild_id]);
    assert_eq!(mid.phase, TaskPhase::Completed);
}

/// Resume: task in Verifying phase goes straight to re-verification, not re-execution.
#[tokio::test]
async fn resume_verifying_skips_execution() {
    let mock = MockBuilder::new()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![child_id];

    let mut child = Task::new(
        child_id,
        Some(root_id),
        "child".into(),
        vec!["child passes".into()],
        1,
    );
    child.path = Some(TaskPath::Leaf);
    child.current_model = Some(Model::Haiku);
    child.phase = TaskPhase::Verifying;

    state.insert(root);
    state.insert(child);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    assert_eq!(state.get(child_id).unwrap().phase, TaskPhase::Completed);
    assert!(state.get(child_id).unwrap().attempts.is_empty());
}

/// Custom `max_depth`: child at depth limit is forced to Leaf without assess.
/// Subsumes the previous `depth_cap_forces_leaf` test.
#[tokio::test]
async fn custom_max_depth_forces_leaf() {
    let mock = MockBuilder::new()
        .decompose_one()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let limits = LimitsConfig {
        max_depth: 2,
        ..LimitsConfig::default()
    };

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let root = Task::new(root_id, None, "deep root".into(), vec!["passes".into()], 1);
    state.insert(root);
    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx).with_limits(limits);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child_id = state.get(root_id).unwrap().subtask_ids[0];
    let child = state.get(child_id).unwrap();
    assert_eq!(child.path, Some(TaskPath::Leaf));
    assert_eq!(child.depth, 2);
}

/// Leaf reports discoveries -> stored on task -> checkpoint called -> sibling sees them.
#[tokio::test]
async fn discoveries_propagated_to_checkpoint() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf().assess_leaf();
    mb.leaf_success_with_discoveries(vec![
        "API uses v2 format".into(),
        "cache layer found".into(),
    ]);
    mb.checkpoint_proceed();
    mb.leaf_success();
    mb.verify_passes(3).file_review_passes(2);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child_a_id = state.get(root_id).unwrap().subtask_ids[0];
    let child_a = state.get(child_a_id).unwrap();
    assert_eq!(
        child_a.discoveries,
        vec!["API uses v2 format", "cache layer found"]
    );

    let mut found_discoveries_event = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::DiscoveriesRecorded { task_id, count } if task_id == child_a_id && count == 2)
        {
            found_discoveries_event = true;
        }
    }
    assert!(
        found_discoveries_event,
        "DiscoveriesRecorded event not found"
    );
}

/// Branch fix loop: root verification fails -> fix subtask created -> re-verify passes.
#[tokio::test]
async fn branch_fix_creates_subtasks() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed")
        .fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_pass()
        .file_review_passes(3)
        .build();
    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.subtask_ids.len(), 2);
    assert_eq!(root.verification_fix_rounds, 1);
    assert_eq!(root.phase, TaskPhase::Completed);

    let fix_id = root.subtask_ids[1];
    let fix_task = state.get(fix_id).unwrap();
    assert!(fix_task.is_fix_task);
    assert_eq!(fix_task.phase, TaskPhase::Completed);
}

/// Branch fix loop: non-root branch exhausts 3 rounds -> terminal failure.
#[tokio::test]
async fn branch_fix_round_budget() {
    let mut mb = MockBuilder::new();
    mb.decompose(DecompositionResult {
        subtasks: vec![SubtaskSpec {
            goal: "mid branch".into(),
            verification_criteria: vec!["mid passes".into()],
            magnitude_estimate: MagnitudeEstimate::Medium,
        }],
        rationale: "one mid branch".into(),
    });
    mb.assess_branch().decompose_one();
    mb.assess_leaf().leaf_success().verify_pass();
    mb.verify_fail("mid check failed");

    for _ in 0..3 {
        mb.fix_subtask_one()
            .assess_leaf()
            .leaf_success()
            .verify_pass()
            .verify_fail("still failing");
    }
    mb.recovery_unrecoverable();
    mb.file_review_passes(4);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let mid_id = state.get(root_id).unwrap().subtask_ids[0];
    let mid = state.get(mid_id).unwrap();
    assert_eq!(mid.verification_fix_rounds, 3);
    assert_eq!(mid.phase, TaskPhase::Failed);
    assert_eq!(mid.subtask_ids.len(), 4);
}

/// Fix tasks (leaf or branch) fail immediately to prevent recursive fix-within-fix.
/// Merged from `branch_fix_subtasks_no_recursive_fix` and `leaf_fix_subtask_no_recursive_fix_loop`.
#[tokio::test]
async fn fix_subtasks_no_recursive_fix() {
    // --- Part 1: Leaf fix subtask that fails verification must NOT enter leaf fix loop ---
    let mut mb = MockBuilder::new();
    // Root branches, original child succeeds, root verify fails.
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    // Fix round 1: leaf fix subtask succeeds but verification fails.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_fail("fix leaf failed");
    mb.verify_fail("root still failing");
    // Fix round 2: simple fix subtask succeeds and verification passes.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_pass();
    mb.file_review_passes(2);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 2);
    let fix1_id = root.subtask_ids[1];
    let fix1 = state.get(fix1_id).unwrap();
    assert!(fix1.is_fix_task);
    assert_eq!(fix1.path, Some(TaskPath::Leaf));
    assert_eq!(fix1.phase, TaskPhase::Failed);
    assert_eq!(fix1.fix_attempts.len(), 0);

    // --- Part 2: Branch fix subtask that fails verification must NOT enter branch_fix_loop ---
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    // Fix round 1: branch fix subtask.
    mb.fix_subtask_one().assess_branch().decompose_one();
    mb.assess_leaf().leaf_success().verify_pass();
    mb.verify_fail("branch fix subtask failed");
    mb.verify_fail("root still failing");
    // Fix round 2: simple fix subtask succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_pass();
    mb.file_review_passes(3);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 2);
    let fix1_id = root.subtask_ids[1];
    let fix1 = state.get(fix1_id).unwrap();
    assert!(fix1.is_fix_task);
    assert_eq!(fix1.path, Some(TaskPath::Branch));
    assert_eq!(fix1.phase, TaskPhase::Failed);
    assert_eq!(
        fix1.verification_fix_rounds, 0,
        "branch fix subtask should not enter its own branch_fix_loop"
    );
}

/// Resume mid-fix-loop: pre-existing `fix_attempts` are counted so `retries_at_tier` is correct.
#[tokio::test]
async fn leaf_fix_persists_and_resumes() {
    let mock = MockBuilder::new()
        .verify_fail("still broken")
        .fix_leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![child_id];

    let mut child = Task::new(
        child_id,
        Some(root_id),
        "child".into(),
        vec!["child passes".into()],
        1,
    );
    child.path = Some(TaskPath::Leaf);
    child.current_model = Some(Model::Haiku);
    child.phase = TaskPhase::Verifying;
    child.fix_attempts = vec![
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("fail1".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("fail2".into()),
        },
    ];

    state.insert(root);
    state.insert(child);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.fix_attempts.len(), 3);
    assert!(child.fix_attempts[2].succeeded);
    assert_eq!(child.fix_attempts[2].model, Model::Haiku);
}

/// Resume fix loop with exhausted tier: escalates immediately without extra attempt.
#[tokio::test]
async fn leaf_fix_resume_escalates_immediately_when_tier_exhausted() {
    let mock = MockBuilder::new()
        .verify_fail("still broken")
        .fix_leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![child_id];

    let mut child = Task::new(
        child_id,
        Some(root_id),
        "child".into(),
        vec!["child passes".into()],
        1,
    );
    child.path = Some(TaskPath::Leaf);
    child.current_model = Some(Model::Haiku);
    child.phase = TaskPhase::Verifying;
    child.fix_attempts = vec![
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("f1".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("f2".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("f3".into()),
        },
    ];

    state.insert(root);
    state.insert(child);

    let (tx, mut rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.fix_attempts.len(), 4);
    assert_eq!(child.fix_attempts[3].model, Model::Sonnet);
    assert!(child.fix_attempts[3].succeeded);

    let mut saw_escalation = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            Event::FixModelEscalated {
                from: Model::Haiku,
                to: Model::Sonnet,
                ..
            }
        ) {
            saw_escalation = true;
        }
    }
    assert!(saw_escalation, "FixModelEscalated Haiku->Sonnet expected");
}

/// Branch fix loop: root gets 4th round at Opus after 3 Sonnet rounds fail.
#[tokio::test]
async fn branch_fix_root_opus_round() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");

    for round in 1..=4 {
        mb.fix_subtask_one()
            .assess_leaf()
            .leaf_success()
            .verify_pass();
        if round < 4 {
            mb.verify_fail("root still failing");
        } else {
            mb.verify_pass();
        }
    }
    mb.file_review_passes(5);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 4);
    assert_eq!(root.phase, TaskPhase::Completed);
    assert_eq!(root.subtask_ids.len(), 5);

    let mut branch_fix_rounds: Vec<(u32, Model)> = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let Event::BranchFixRound {
            task_id,
            round,
            model,
        } = event
        {
            if task_id == root_id {
                branch_fix_rounds.push((round, model));
            }
        }
    }
    assert_eq!(branch_fix_rounds.len(), 4);
    assert_eq!(branch_fix_rounds[0], (1, Model::Sonnet));
    assert_eq!(branch_fix_rounds[1], (2, Model::Sonnet));
    assert_eq!(branch_fix_rounds[2], (3, Model::Sonnet));
    assert_eq!(branch_fix_rounds[3], (4, Model::Opus));
}

// -----------------------------------------------------------------------
// Recovery re-decomposition tests
// -----------------------------------------------------------------------

/// Child A fails -> incremental recovery -> recovery subtask succeeds -> child B runs -> success.
#[tokio::test]
async fn recovery_incremental_creates_subtasks() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf().leaf_failures(9, "A failed");
    mb.recovery_recoverable("retry differently")
        .recovery_plan_incremental();
    mb.assess_leaf().leaf_success().verify_pass();
    mb.assess_leaf().leaf_success().verify_pass();
    mb.verify_pass();
    mb.file_review_passes(3);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.subtask_ids.len(), 3);
    assert_eq!(root.recovery_rounds, 1);

    let first_child_id = root.subtask_ids[0];
    assert_eq!(state.get(first_child_id).unwrap().phase, TaskPhase::Failed);

    let second_child_id = root.subtask_ids[1];
    assert_eq!(
        state.get(second_child_id).unwrap().phase,
        TaskPhase::Completed
    );

    let recovery_id = root.subtask_ids[2];
    let recovery_task = state.get(recovery_id).unwrap();
    assert_eq!(recovery_task.phase, TaskPhase::Completed);
    assert!(!recovery_task.is_fix_task);
}

/// Full re-decomposition with 3 children: child A completed, child B fails,
/// child C (pending) gets superseded. Recovery subtask succeeds.
/// Subsumes the previous `recovery_full_redecomposition_skips_pending` test.
#[tokio::test]
async fn recovery_full_redecomp_preserves_completed_siblings() {
    let mut mb = MockBuilder::new();
    mb.decompose_three();
    // Child A: succeeds.
    mb.assess_leaf().leaf_success().verify_pass();
    // Child B: fails terminally.
    mb.assess_leaf().leaf_failures(9, "B failed");
    // Recovery: full re-decomposition.
    mb.recovery_recoverable("redo").recovery_plan_full();
    // Recovery subtask: succeeds.
    mb.assess_leaf().leaf_success().verify_pass();
    // Root verification passes.
    mb.verify_pass();
    mb.file_review_passes(3);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.subtask_ids.len(), 4);

    // A: Completed (untouched by full re-decomposition).
    assert_eq!(
        state.get(root.subtask_ids[0]).unwrap().phase,
        TaskPhase::Completed
    );
    // B: Failed (the one that triggered recovery).
    assert_eq!(
        state.get(root.subtask_ids[1]).unwrap().phase,
        TaskPhase::Failed
    );
    // C: Failed (superseded -- was Pending when full re-decomposition ran).
    assert_eq!(
        state.get(root.subtask_ids[2]).unwrap().phase,
        TaskPhase::Failed
    );
    // Recovery subtask: Completed.
    assert_eq!(
        state.get(root.subtask_ids[3]).unwrap().phase,
        TaskPhase::Completed
    );
}

/// Recovery rounds exhausted (2 rounds) -> parent fails.
#[tokio::test]
async fn recovery_round_limit_exhausted() {
    let mut mb = MockBuilder::new();
    mb.decompose(DecompositionResult {
        subtasks: vec![SubtaskSpec {
            goal: "child A".into(),
            verification_criteria: vec!["A passes".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "one subtask".into(),
    });
    mb.assess_leaf().leaf_failures(9, "A failed");
    mb.recovery_recoverable("try again")
        .recovery_plan_incremental();
    mb.assess_leaf().leaf_failures(9, "recovery 1 failed");
    mb.recovery_recoverable("try again")
        .recovery_plan_incremental();
    mb.assess_leaf().leaf_failures(9, "recovery 2 failed");

    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted"))
    );
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 2);
}

/// Fix tasks do not attempt recovery (prevents recursive recovery chains).
#[tokio::test]
async fn recovery_not_attempted_for_fix_tasks() {
    let mock = MockBuilder::new().build();
    let mut state = EpicState::new();

    let root_id = state.next_task_id();
    let mut root = Task::new(root_id, None, "fix parent".into(), vec!["passes".into()], 0);
    root.is_fix_task = true;
    root.path = Some(TaskPath::Branch);
    state.insert(root);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch
        .attempt_recovery(&mut state, root_id, "child broke")
        .await
        .unwrap();
    assert!(result.is_some());
    assert!(matches!(result.unwrap(), TaskOutcome::Failed { .. }));
}

/// `assess_recovery` returns None -> child failure propagates immediately.
#[tokio::test]
async fn recovery_not_attempted_when_unrecoverable() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_failures(9, "terminal")
        .recovery_unrecoverable()
        .build();
    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 0);
}

/// Recovery round counter persists across resume.
#[tokio::test]
async fn recovery_rounds_persisted() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_failures(9, "A failed");
    mb.recovery_recoverable("try again")
        .recovery_plan_incremental();
    mb.assess_leaf().leaf_success().verify_pass();
    mb.verify_pass();
    mb.file_review_passes(2);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.recovery_rounds, 1);

    let json = serde_json::to_string(&root).unwrap();
    let restored: Task = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.recovery_rounds, 1);
}

/// Recovery plan with empty subtask list -> treated as failed recovery.
#[tokio::test]
async fn recovery_empty_plan_fails() {
    let mut mb = MockBuilder::new();
    mb.decompose_one().assess_leaf().leaf_failures(9, "broke");
    mb.recovery_recoverable("try something");
    mb.recovery_plan(RecoveryPlan {
        full_redecomposition: false,
        subtasks: vec![],
        rationale: "empty plan".into(),
    });
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { reason } if reason.contains("no subtasks")));
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);
}

/// Recovery events are emitted correctly.
#[tokio::test]
async fn recovery_emits_events() {
    let mut mb = MockBuilder::new();
    mb.decompose_one().assess_leaf().leaf_failures(9, "broke");
    mb.recovery_recoverable("fix it")
        .recovery_plan_incremental();
    mb.assess_leaf().leaf_success().verify_pass();
    mb.verify_pass();
    mb.file_review_passes(2);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let mut saw_recovery_started = false;
    let mut saw_recovery_plan = false;
    let mut saw_recovery_subtasks = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            Event::RecoveryStarted { task_id, round } => {
                assert_eq!(task_id, root_id);
                assert_eq!(round, 1);
                saw_recovery_started = true;
            }
            Event::RecoveryPlanSelected {
                task_id,
                ref approach,
            } => {
                assert_eq!(task_id, root_id);
                assert_eq!(approach, "incremental");
                saw_recovery_plan = true;
            }
            Event::RecoverySubtasksCreated {
                task_id,
                count,
                round,
            } => {
                assert_eq!(task_id, root_id);
                assert_eq!(count, 1);
                assert_eq!(round, 1);
                saw_recovery_subtasks = true;
            }
            _ => {}
        }
    }
    assert!(saw_recovery_started);
    assert!(saw_recovery_plan);
    assert!(saw_recovery_subtasks);
}

/// Checkpoint adjust: guidance stored on parent, visible to sibling B via context.
#[tokio::test]
async fn checkpoint_adjust_stores_guidance() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf().assess_leaf();
    mb.leaf_success_with_discoveries(vec!["use API v2".into()]);
    mb.checkpoint_adjust("switch to API v2 format");
    mb.leaf_success();
    mb.verify_passes(3);
    mb.file_review_passes(3);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(
        root.checkpoint_guidance.as_deref(),
        Some("switch to API v2 format")
    );

    let mut saw_adjust = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::CheckpointAdjust { task_id } if task_id == root_id) {
            saw_adjust = true;
        }
    }
    assert!(saw_adjust, "CheckpointAdjust event not found");
}

/// Checkpoint escalate: triggers recovery machinery.
#[tokio::test]
async fn checkpoint_escalate_triggers_recovery() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["approach is wrong".into()]);
    mb.checkpoint_escalate();
    mb.recovery_recoverable("switch approach");
    mb.recovery_plan(RecoveryPlan {
        full_redecomposition: false,
        subtasks: vec![SubtaskSpec {
            goal: "recovery child".into(),
            verification_criteria: vec!["recovery passes".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "fix approach".into(),
    });
    mb.assess_leaf().leaf_success();
    mb.assess_leaf().leaf_success();
    mb.verify_passes(4);
    mb.file_review_passes(4);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);

    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
            saw_escalate = true;
        }
        if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
            saw_recovery_started = true;
        }
    }
    assert!(saw_escalate, "CheckpointEscalate event not found");
    assert!(saw_recovery_started, "RecoveryStarted event not found");
}

/// Checkpoint escalate when recovery is not possible: propagates failure.
#[tokio::test]
async fn checkpoint_escalate_unrecoverable_fails() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["fatal issue".into()]);
    mb.verify_pass();
    mb.checkpoint_escalate();
    mb.recovery_unrecoverable();
    mb.file_review_passes(1);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));
}

/// Checkpoint agent error treated as Proceed (best-effort).
#[tokio::test]
async fn checkpoint_agent_error_treated_as_proceed() {
    let mut mb = MockBuilder::new();
    mb.decompose_one();
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["something interesting".into()]);
    mb.checkpoint_error("simulated LLM failure");
    mb.verify_passes(2);
    mb.file_review_passes(2);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event, Event::CheckpointAdjust { .. }),
            "unexpected CheckpointAdjust event after agent error"
        );
        assert!(
            !matches!(event, Event::CheckpointEscalate { .. }),
            "unexpected CheckpointEscalate event after agent error"
        );
    }

    assert!(
        state.get(root_id).unwrap().checkpoint_guidance.is_none(),
        "checkpoint_guidance should be None when agent errors out"
    );
}

/// Checkpoint guidance persisted and survives serialization round-trip.
#[tokio::test]
async fn checkpoint_guidance_persisted() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf().assess_leaf();
    mb.leaf_success_with_discoveries(vec!["found issue".into()]);
    mb.checkpoint_adjust("use new approach");
    mb.leaf_success();
    mb.verify_passes(3);
    mb.file_review_passes(3);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let json = serde_json::to_string(&state).unwrap();
    let restored: EpicState = serde_json::from_str(&json).unwrap();
    assert_eq!(
        restored
            .get(root_id)
            .unwrap()
            .checkpoint_guidance
            .as_deref(),
        Some("use new approach")
    );
}

#[tokio::test]
async fn checkpoint_multiple_adjusts_accumulates_guidance() {
    let mut mb = MockBuilder::new();
    mb.decompose_three();
    for _ in 0..3 {
        mb.assess_leaf();
    }
    mb.leaf_success_with_discoveries(vec!["discovered API v2".into()]);
    mb.checkpoint_adjust("use API v2");
    mb.leaf_success_with_discoveries(vec!["discovered gzip support".into()]);
    mb.checkpoint_adjust("also use gzip");
    mb.leaf_success();
    mb.verify_passes(4);
    mb.file_review_passes(4);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    assert_eq!(
        state.get(root_id).unwrap().checkpoint_guidance.as_deref(),
        Some("use API v2\nalso use gzip")
    );

    let mut adjust_count = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::CheckpointAdjust { task_id } if task_id == root_id) {
            adjust_count += 1;
        }
    }
    assert_eq!(
        adjust_count, 2,
        "expected exactly 2 CheckpointAdjust events"
    );
}

/// When a fix task's child discoveries trigger Escalate, recovery is rejected
/// because `attempt_recovery` refuses fix tasks, so the branch fails immediately.
#[tokio::test]
async fn checkpoint_escalate_on_fix_task_fails() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["fatal issue".into()]);
    mb.verify_pass();
    mb.checkpoint_escalate();
    mb.file_review_passes(1);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());

    state.get_mut(root_id).unwrap().is_fix_task = true;

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
            saw_escalate = true;
        }
        if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
            saw_recovery_started = true;
        }
    }
    assert!(saw_escalate, "CheckpointEscalate event not found");
    assert!(
        !saw_recovery_started,
        "RecoveryStarted should not be emitted for fix tasks"
    );
}

#[tokio::test]
async fn checkpoint_guidance_flows_to_child_context() {
    let mock = MockBuilder::new().build();
    let (tx, _rx) = events::event_channel();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let mut root = Task::new(
        root_id,
        None,
        "root goal".into(),
        vec!["root passes".into()],
        0,
    );

    let first_child_id = state.next_task_id();
    let child_a = Task::new(
        first_child_id,
        Some(root_id),
        "child A goal".into(),
        vec!["A passes".into()],
        1,
    );

    let second_child_id = state.next_task_id();
    let child_b = Task::new(
        second_child_id,
        Some(root_id),
        "child B goal".into(),
        vec!["B passes".into()],
        1,
    );

    root.subtask_ids = vec![first_child_id, second_child_id];
    root.checkpoint_guidance = Some("use API v2".into());
    state.insert(root);

    let mut a = child_a;
    a.phase = TaskPhase::Completed;
    state.insert(a);
    state.insert(child_b);

    let orch = Orchestrator::new(mock, tx);
    let ctx = orch.build_context(&state, second_child_id).unwrap();

    assert_eq!(
        ctx.checkpoint_guidance.as_deref(),
        Some("use API v2"),
        "checkpoint guidance from parent should flow into child context"
    );
}

/// Checkpoint escalation when recovery rounds are already at `max_recovery_rounds`
/// results in immediate failure.
#[tokio::test]
async fn checkpoint_escalate_recovery_rounds_exhausted() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["approach is wrong".into()]);
    mb.verify_pass();
    mb.checkpoint_escalate();
    mb.file_review_passes(2);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());

    state.get_mut(root_id).unwrap().recovery_rounds = 2;

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("recovery rounds exhausted")),
        "expected failure with 'recovery rounds exhausted', got: {result:?}"
    );

    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
            saw_escalate = true;
        }
        if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
            saw_recovery_started = true;
        }
    }
    assert!(saw_escalate, "CheckpointEscalate event not found");
    assert!(
        !saw_recovery_started,
        "RecoveryStarted should not be emitted when recovery rounds are exhausted"
    );
}

/// Checkpoint escalate after prior adjust: guidance is cleared before recovery runs.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn checkpoint_escalate_clears_prior_guidance() {
    let mut mb = MockBuilder::new();
    mb.decompose_three();
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["use API v2".into()]);
    mb.checkpoint_adjust("old guidance");
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["approach is fundamentally wrong".into()]);
    mb.checkpoint_escalate();
    mb.recovery_recoverable("fix approach");
    mb.recovery_plan(RecoveryPlan {
        full_redecomposition: false,
        subtasks: vec![SubtaskSpec {
            goal: "recovery child".into(),
            verification_criteria: vec!["recovery passes".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "fix approach".into(),
    });
    mb.assess_leaf().leaf_success();
    mb.assess_leaf().leaf_success();
    mb.verify_passes(5);
    mb.file_review_passes(4);
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    assert!(
        state.get(root_id).unwrap().checkpoint_guidance.is_none(),
        "checkpoint_guidance should be None after escalation clears prior adjust guidance"
    );

    let mut saw_adjust = false;
    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::CheckpointAdjust { task_id } if task_id == root_id) {
            saw_adjust = true;
        }
        if matches!(event, Event::CheckpointEscalate { task_id } if task_id == root_id) {
            saw_escalate = true;
        }
        if matches!(event, Event::RecoveryStarted { task_id, .. } if task_id == root_id) {
            saw_recovery_started = true;
        }
    }
    assert!(saw_adjust, "CheckpointAdjust event not found");
    assert!(saw_escalate, "CheckpointEscalate event not found");
    assert!(saw_recovery_started, "RecoveryStarted event not found");
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);
}

/// Resume mid-leaf-retry: pre-existing attempts are counted so `retries_at_tier` is correct.
#[tokio::test]
async fn leaf_retry_counter_persists_on_resume() {
    let mock = MockBuilder::new()
        .leaf_failed("fail3")
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![child_id];

    let mut child = Task::new(
        child_id,
        Some(root_id),
        "child".into(),
        vec!["child passes".into()],
        1,
    );
    child.path = Some(TaskPath::Leaf);
    child.current_model = Some(Model::Haiku);
    child.phase = TaskPhase::Executing;
    child.attempts = vec![
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("fail1".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("fail2".into()),
        },
    ];

    state.insert(root);
    state.insert(child);

    let (tx, mut rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.attempts.len(), 4);
    assert_eq!(child.attempts[2].model, Model::Haiku);
    assert!(!child.attempts[2].succeeded);
    assert_eq!(child.attempts[3].model, Model::Sonnet);
    assert!(child.attempts[3].succeeded);

    let mut saw_escalation = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            Event::ModelEscalated {
                from: Model::Haiku,
                to: Model::Sonnet,
                ..
            }
        ) {
            saw_escalation = true;
        }
    }
    assert!(
        saw_escalation,
        "ModelEscalated event should be emitted after 3 Haiku failures"
    );
}

/// Resume at Sonnet tier with pre-existing Sonnet attempts.
#[tokio::test]
async fn leaf_retry_counter_resume_at_sonnet_tier() {
    let mock = MockBuilder::new()
        .leaf_failed("sonnet fail3")
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![child_id];

    let mut child = Task::new(
        child_id,
        Some(root_id),
        "child".into(),
        vec!["child passes".into()],
        1,
    );
    child.path = Some(TaskPath::Leaf);
    child.current_model = Some(Model::Sonnet);
    child.phase = TaskPhase::Executing;
    child.attempts = vec![
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("h1".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("h2".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("h3".into()),
        },
        Attempt {
            model: Model::Sonnet,
            succeeded: false,
            error: Some("s1".into()),
        },
        Attempt {
            model: Model::Sonnet,
            succeeded: false,
            error: Some("s2".into()),
        },
    ];

    state.insert(root);
    state.insert(child);

    let (tx, mut rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.attempts.len(), 7);
    assert_eq!(child.attempts[5].model, Model::Sonnet);
    assert!(!child.attempts[5].succeeded);
    assert_eq!(child.attempts[6].model, Model::Opus);
    assert!(child.attempts[6].succeeded);

    let mut saw_escalation = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            Event::ModelEscalated {
                from: Model::Sonnet,
                to: Model::Opus,
                ..
            }
        ) {
            saw_escalation = true;
        }
    }
    assert!(saw_escalation, "ModelEscalated Sonnet->Opus event expected");
}

/// Resume with retries exhausted at current tier: escalates immediately.
#[tokio::test]
async fn leaf_retry_resume_escalates_immediately_when_tier_exhausted() {
    let mock = MockBuilder::new()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let child_id = state.next_task_id();

    let mut root = Task::new(root_id, None, "root".into(), vec!["root passes".into()], 0);
    root.path = Some(TaskPath::Branch);
    root.current_model = Some(Model::Sonnet);
    root.phase = TaskPhase::Executing;
    root.subtask_ids = vec![child_id];

    let mut child = Task::new(
        child_id,
        Some(root_id),
        "child".into(),
        vec!["child passes".into()],
        1,
    );
    child.path = Some(TaskPath::Leaf);
    child.current_model = Some(Model::Haiku);
    child.phase = TaskPhase::Executing;
    child.attempts = vec![
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("f1".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("f2".into()),
        },
        Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("f3".into()),
        },
    ];

    state.insert(root);
    state.insert(child);

    let (tx, mut rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.attempts.len(), 4);
    assert_eq!(child.attempts[3].model, Model::Sonnet);
    assert!(child.attempts[3].succeeded);

    let mut saw_escalation = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event,
            Event::ModelEscalated {
                from: Model::Haiku,
                to: Model::Sonnet,
                ..
            }
        ) {
            saw_escalation = true;
        }
    }
    assert!(saw_escalation, "ModelEscalated Haiku->Sonnet expected");
}

/// Leaf retry attempts are persisted to disk via `checkpoint_save`.
#[tokio::test]
async fn leaf_retry_attempts_persisted_to_disk() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_failed("first try failed")
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let state_path = tmp.path().to_path_buf();

    let (mut orch, mut state, root_id, _rx) = make_orchestrator(mock);
    orch.services.state_path = Some(state_path.clone());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let loaded_state = EpicState::load(&state_path).unwrap();
    let child_id = state.get(root_id).unwrap().subtask_ids[0];
    let child = loaded_state.get(child_id).unwrap();
    assert_eq!(child.attempts.len(), 2);
    assert!(!child.attempts[0].succeeded);
    assert!(child.attempts[1].succeeded);
}

// -----------------------------------------------------------------------
// Config wiring tests
// -----------------------------------------------------------------------

/// Custom `max_recovery_rounds`=1: recovery attempted once, refused on second failure.
#[tokio::test]
async fn custom_max_recovery_rounds_limits_recovery() {
    let mut mb = MockBuilder::new();
    mb.decompose(DecompositionResult {
        subtasks: vec![SubtaskSpec {
            goal: "child A".into(),
            verification_criteria: vec!["A passes".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "one subtask".into(),
    });
    mb.assess_leaf().leaf_failures(9, "A failed");
    mb.recovery_recoverable("try again")
        .recovery_plan_incremental();
    mb.assess_leaf().leaf_failures(9, "recovery failed");

    let limits = LimitsConfig {
        max_recovery_rounds: 1,
        ..LimitsConfig::default()
    };

    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let orch = orch.with_limits(limits);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted"))
    );
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);
}

/// Custom `root_fix_rounds`=1: root verification fails -> 1 fix round -> still fails -> task fails.
#[tokio::test]
async fn custom_root_fix_rounds_limits_fix_attempts() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass();
    mb.verify_fail("root still failing");
    mb.file_review_passes(2);

    let limits = LimitsConfig {
        root_fix_rounds: 1,
        ..LimitsConfig::default()
    };

    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let orch = orch.with_limits(limits);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 1);
    assert_eq!(root.phase, TaskPhase::Failed);
}

/// Custom `branch_fix_rounds`=1: non-root branch verification fails -> 1 fix round -> fails.
#[tokio::test]
async fn custom_branch_fix_rounds_limits_fix_attempts() {
    let mut mb = MockBuilder::new();
    mb.decompose_one();
    mb.assess_branch();
    mb.decompose(DecompositionResult {
        subtasks: vec![SubtaskSpec {
            goal: "grandchild".into(),
            verification_criteria: vec!["gc passes".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "one grandchild".into(),
    });
    mb.assess_leaf().leaf_success().verify_pass();
    mb.verify_fail("branch check failed");
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass();
    mb.verify_fail("branch still failing");
    mb.recovery_unrecoverable();
    mb.file_review_passes(2);

    let limits = LimitsConfig {
        branch_fix_rounds: 1,
        ..LimitsConfig::default()
    };

    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let orch = orch.with_limits(limits);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let child_id = state.get(root_id).unwrap().subtask_ids[0];
    let child = state.get(child_id).unwrap();
    assert_eq!(child.verification_fix_rounds, 1);
    assert_eq!(child.phase, TaskPhase::Failed);
}

/// `retry_budget`=0 is clamped to 1: leaf still gets at least one attempt.
#[tokio::test]
async fn zero_retry_budget_clamped_to_one() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_failed("haiku failed")
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();

    let limits = LimitsConfig {
        retry_budget: 0,
        ..LimitsConfig::default()
    };

    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let orch = orch.with_limits(limits);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let child_id = state.get(root_id).unwrap().subtask_ids[0];
    let child = state.get(child_id).unwrap();
    assert_eq!(child.attempts.len(), 2);
    assert_eq!(child.current_model, Some(Model::Sonnet));
}

/// Branch decompose receives the model from assessment.
#[tokio::test]
async fn decompose_model_from_assessment() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_passes(2)
        .file_review_passes(2)
        .build();
    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let captured = orch.services.agent.decompose_models.lock().unwrap().clone();
    assert_eq!(captured[0], Model::Sonnet);
}

// ---- Task limit cap tests ----

/// Decomposition fails gracefully when total task limit would be exceeded.
#[tokio::test]
async fn task_limit_blocks_decomposition() {
    let mut mb = MockBuilder::new();
    mb.decompose(DecompositionResult {
        subtasks: vec![
            SubtaskSpec {
                goal: "child A".into(),
                verification_criteria: vec!["A passes".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            },
            SubtaskSpec {
                goal: "child B".into(),
                verification_criteria: vec!["B passes".into()],
                magnitude_estimate: MagnitudeEstimate::Small,
            },
        ],
        rationale: "two subtasks".into(),
    });

    let (mut orch, mut state, root_id, mut rx) = make_orchestrator(mb.build());
    orch.services.limits.max_total_tasks = 2;
    let result = orch.run(&mut state, root_id).await.unwrap();
    let TaskOutcome::Failed { reason } = &result else {
        panic!("expected TaskOutcome::Failed, got {result:?}");
    };
    assert!(
        reason.contains("task limit reached"),
        "unexpected reason: {reason}"
    );

    let mut limit_events: Vec<TaskId> = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let Event::TaskLimitReached { task_id } = event {
            limit_events.push(task_id);
        }
    }
    assert_eq!(
        limit_events.len(),
        1,
        "expected exactly one TaskLimitReached event"
    );
    assert_eq!(limit_events[0], root_id);
}

/// Fix subtask creation blocked by task limit.
#[tokio::test]
async fn task_limit_blocks_fix_subtasks() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass();
    mb.verify_fail("root verification failed");
    mb.fix_subtask_one();
    mb.file_review_passes(1);

    let (mut orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    orch.services.limits.max_total_tasks = 2;
    let result = orch.run(&mut state, root_id).await.unwrap();
    let TaskOutcome::Failed { reason } = &result else {
        panic!("expected TaskOutcome::Failed, got {result:?}");
    };
    assert!(
        reason.contains("task limit reached"),
        "unexpected reason: {reason}"
    );
}

/// Recovery subtask creation blocked by task limit.
#[tokio::test]
async fn task_limit_blocks_recovery_subtasks() {
    let mut mb = MockBuilder::new();
    mb.decompose_one().assess_leaf().leaf_failures(9, "boom");
    mb.recovery_recoverable("retry with different approach");
    mb.recovery_plan(RecoveryPlan {
        full_redecomposition: false,
        subtasks: vec![SubtaskSpec {
            goal: "recovery child".into(),
            verification_criteria: vec!["recovers".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "recovery".into(),
    });

    let (mut orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    orch.services.limits.max_total_tasks = 2;
    let result = orch.run(&mut state, root_id).await.unwrap();
    let TaskOutcome::Failed { reason } = &result else {
        panic!("expected TaskOutcome::Failed, got {result:?}");
    };
    assert!(
        reason.contains("task limit reached"),
        "unexpected reason: {reason}"
    );
}

/// Recovery subtasks inherit parent's `recovery_rounds` (no fresh budget).
#[tokio::test]
async fn recovery_depth_inherited_not_fresh() {
    let mut mb = MockBuilder::new();
    mb.decompose_one().assess_leaf().leaf_failures(9, "boom");
    mb.recovery_recoverable("retry");
    mb.recovery_plan(RecoveryPlan {
        full_redecomposition: false,
        subtasks: vec![SubtaskSpec {
            goal: "recovery branch".into(),
            verification_criteria: vec!["recovers".into()],
            magnitude_estimate: MagnitudeEstimate::Small,
        }],
        rationale: "recovery".into(),
    });
    mb.assess_leaf().leaf_success();
    mb.verify_passes(2);
    mb.file_review_passes(2);

    let (mut orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    orch.services.limits.max_total_tasks = 100;
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.recovery_rounds, 1);
    let recovery_child_id = *root.subtask_ids.last().unwrap();
    let recovery_child = state.get(recovery_child_id).unwrap();
    assert_eq!(
        recovery_child.recovery_rounds, 1,
        "recovery subtask should inherit parent's recovery_rounds, not start at 0"
    );
}

/// Inherited recovery budget blocks a second recovery round.
#[tokio::test]
async fn recovery_inherited_budget_blocks_second_recovery() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_failures(9, "child failed");
    mb.recovery_recoverable("retry").recovery_plan_incremental();
    mb.assess_leaf().leaf_failures(9, "recovery child failed");

    let limits = LimitsConfig {
        max_recovery_rounds: 1,
        ..LimitsConfig::default()
    };

    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let orch = orch.with_limits(limits);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("recovery rounds exhausted")),
        "expected recovery-exhausted failure, got {result:?}"
    );

    let root = state.get(root_id).unwrap();
    assert_eq!(root.recovery_rounds, 1);
    let recovery_child_id = *root.subtask_ids.last().unwrap();
    let recovery_child = state.get(recovery_child_id).unwrap();
    assert_eq!(
        recovery_child.recovery_rounds, 1,
        "recovery child should inherit parent's recovery_rounds (1)"
    );
}

/// `max_total_tasks = 0` is clamped to 1.
#[tokio::test]
async fn max_total_tasks_zero_clamped_blocks_decomposition() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_passes(2)
        .build();

    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let orch = orch.with_limits(LimitsConfig {
        max_total_tasks: 0,
        ..LimitsConfig::default()
    });
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("task limit reached")),
        "expected task limit failure after clamping 0->1, got {result:?}"
    );
}

/// Exact boundary: `max_total_tasks = 3`, root + 2 children = 3, succeeds.
#[tokio::test]
async fn task_limit_exact_boundary_permits() {
    let mut mb = MockBuilder::new();
    mb.decompose_two();
    for _ in 0..2 {
        mb.assess_leaf().leaf_success();
    }
    mb.verify_passes(3).file_review_passes(3);

    let (mut orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    orch.services.limits.max_total_tasks = 3;
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);
    assert_eq!(state.task_count(), 3);
}

/// Branch fix loop: `design_fix_subtasks()` returns Err on round 1, succeeds on round 2.
#[tokio::test]
async fn branch_fix_design_error_retries() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    mb.fix_subtask_error(TaskId(0), "LLM timeout");
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_pass();
    mb.file_review_passes(2);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.phase, TaskPhase::Completed);
    assert_eq!(root.verification_fix_rounds, 2);
}

/// Branch fix loop: `verify()` returns Err on round 1 re-verification, passes on round 2.
#[tokio::test]
async fn branch_fix_verify_error_retries() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    // Round 1: fix subtask succeeds, root verify returns Err.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass(); // fix subtask verification
    mb.verify_errors_sequence(TaskId(0), vec![None, Some("transient verify error".into())]);
    // Round 2: fix subtask succeeds, root verify passes.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // fix subtask verification
        .verify_pass(); // root re-verify passes
    mb.file_review_passes(3);
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.phase, TaskPhase::Completed);
    assert_eq!(root.verification_fix_rounds, 2);
}

/// All `root_fix_rounds` consumed by `design_fix_subtasks` errors -> Failed.
#[tokio::test]
async fn branch_fix_design_error_exhausts_budget() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    mb.fix_subtask_errors(
        TaskId(0),
        vec![
            Some("LLM timeout round 1".into()),
            Some("LLM timeout round 2".into()),
        ],
    );
    mb.recovery_unrecoverable();
    mb.file_review_passes(1);

    let limits = LimitsConfig {
        root_fix_rounds: 2,
        ..LimitsConfig::default()
    };

    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let orch = orch.with_limits(limits);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let root = state.get(root_id).unwrap();
    assert_eq!(root.phase, TaskPhase::Failed);
    assert_eq!(root.verification_fix_rounds, 2);
}

/// Initial `verify()` returning `Err` must propagate as `Err` from `run()`.
#[tokio::test]
async fn initial_verify_error_is_fatal() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_error(TaskId(1), "agent crashed")
        .build();
    let (orch, mut state, root_id, _rx) = make_orchestrator(mock);
    let result = orch.run(&mut state, root_id).await;
    assert!(result.is_err());
}

/// When all non-fix children are Failed, `execute_branch` must return Failure.
#[tokio::test]
async fn branch_fails_when_all_children_failed() {
    let mock = MockBuilder::new().build();
    let mut state = EpicState::new();

    let root_id = state.next_task_id();
    let mut root = Task::new(
        root_id,
        None,
        "root goal".into(),
        vec!["root passes".into()],
        0,
    );
    root.path = Some(TaskPath::Branch);
    root.phase = TaskPhase::Executing;

    let child_a = state.next_task_id();
    let mut a = Task::new(
        child_a,
        Some(root_id),
        "child A".into(),
        vec!["A passes".into()],
        1,
    );
    a.phase = TaskPhase::Failed;
    a.path = Some(TaskPath::Leaf);

    let child_b = state.next_task_id();
    let mut b = Task::new(
        child_b,
        Some(root_id),
        "child B".into(),
        vec!["B passes".into()],
        1,
    );
    b.phase = TaskPhase::Failed;
    b.path = Some(TaskPath::Leaf);

    root.subtask_ids = vec![child_a, child_b];
    state.insert(root);
    state.insert(a);
    state.insert(b);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("all non-fix children failed")),
        "expected Failure when all children failed, got: {result:?}"
    );
}

/// Branch with a mix of failed and completed non-fix children should still report Success.
#[tokio::test]
async fn branch_succeeds_when_some_children_completed() {
    let mock = MockBuilder::new().verify_pass().build();
    let mut state = EpicState::new();

    let root_id = state.next_task_id();
    let mut root = Task::new(
        root_id,
        None,
        "root goal".into(),
        vec!["root passes".into()],
        0,
    );
    root.path = Some(TaskPath::Branch);
    root.phase = TaskPhase::Executing;

    let child_a = state.next_task_id();
    let mut a = Task::new(
        child_a,
        Some(root_id),
        "child A".into(),
        vec!["A passes".into()],
        1,
    );
    a.phase = TaskPhase::Completed;
    a.path = Some(TaskPath::Leaf);

    let child_b = state.next_task_id();
    let mut b = Task::new(
        child_b,
        Some(root_id),
        "child B".into(),
        vec!["B passes".into()],
        1,
    );
    b.phase = TaskPhase::Failed;
    b.path = Some(TaskPath::Leaf);

    root.subtask_ids = vec![child_a, child_b];
    state.insert(root);
    state.insert(a);
    state.insert(b);

    let (tx, _rx) = events::event_channel();
    let orch = Orchestrator::new(mock, tx);

    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);
}

// -----------------------------------------------------------------------
// File-level review tests
// -----------------------------------------------------------------------

/// Branch tasks skip file-level review, completing directly after verification.
#[tokio::test]
async fn branch_skips_file_level_review() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_pass()
        .file_review_passes(2)
        .build();
    let (orch, mut state, root_id, mut rx) = make_orchestrator(mock);
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let mut root_review_events = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, Event::FileLevelReviewCompleted { task_id, .. } if task_id == root_id) {
            root_review_events += 1;
        }
    }
    assert_eq!(
        root_review_events, 0,
        "branch tasks should not emit FileLevelReviewCompleted"
    );
}

/// Fix task that fails file-level review -> fails immediately (no fix loop).
#[tokio::test]
async fn fix_task_file_review_fail_no_fix_loop() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .verify_fail("root check failed");
    // Original child file review.
    mb.file_review_pass();
    // Fix round 1: fix subtask passes verification but fails file review.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_fail("fix incomplete");
    mb.verify_fail("root still failing");
    // Fix round 2: succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass();
    mb.verify_pass();
    let (orch, mut state, root_id, _rx) = make_orchestrator(mb.build());
    let result = orch.run(&mut state, root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 2);

    let fix1_id = root.subtask_ids[1];
    let fix1 = state.get(fix1_id).unwrap();
    assert!(fix1.is_fix_task);
    assert_eq!(fix1.phase, TaskPhase::Failed);
    assert_eq!(
        fix1.fix_attempts.len(),
        0,
        "fix task should not enter fix loop on file-level review failure"
    );
}
