use crate::config::project::LimitsConfig;
use crate::events::{Event, EventLog};
use crate::state::EpicState;
use crate::store::EpicStore;
use crate::task::branch::{DecompositionResult, SubtaskSpec};
use crate::task::{
    Attempt, MagnitudeEstimate, Model, RecoveryPlan, Task, TaskId, TaskOutcome, TaskPath, TaskPhase,
};
use crate::test_support::{MockAgentService, MockBuilder};
use std::sync::Arc;

type TestOrchestrator = cue::Orchestrator<EpicStore<MockAgentService>, EventLog>;

/// Build a `cue::Orchestrator<EpicStore<MockAgentService>, EventLog>` with a single root task.
/// Returns the orchestrator, a cloned Arc to the mock (for post-run inspection),
/// the root `TaskId`, and the event log.
///
/// Root gets TaskId(0); subtasks get sequential IDs (TaskId(1), TaskId(2), ...) in creation order.
fn make_orchestrator(
    mock: MockAgentService,
) -> (TestOrchestrator, Arc<MockAgentService>, TaskId, EventLog) {
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
    let log = EventLog::new();
    let mock_arc = Arc::new(mock);
    let store = EpicStore::from_state(
        state,
        Arc::clone(&mock_arc),
        log.clone(),
        None,
        LimitsConfig::default(),
        None,
    );
    let orchestrator = cue::Orchestrator::new(store, log.clone());
    (orchestrator, mock_arc, root_id, log)
}

/// Build a `cue::Orchestrator` with a single root task and custom limits on both
/// the store's runtime and the orchestrator.
fn make_orchestrator_with_limits(
    mock: MockAgentService,
    mut limits: LimitsConfig,
) -> (TestOrchestrator, Arc<MockAgentService>, TaskId, EventLog) {
    // Apply the same clamping that Orchestrator::with_limits does, so the
    // store's runtime and the orchestrator see identical values.
    limits.retry_budget = limits.retry_budget.max(1);
    limits.branch_fix_rounds = limits.branch_fix_rounds.max(1);
    limits.root_fix_rounds = limits.root_fix_rounds.max(1);
    limits.max_total_tasks = limits.max_total_tasks.max(1);

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
    let log = EventLog::new();
    let mock_arc = Arc::new(mock);
    let store = EpicStore::from_state(
        state,
        Arc::clone(&mock_arc),
        log.clone(),
        None,
        limits.clone(),
        None,
    );
    let orchestrator = cue::Orchestrator::new(store, log.clone()).with_limits(limits);
    (orchestrator, mock_arc, root_id, log)
}

/// Build a `cue::Orchestrator` from a pre-populated `EpicState` (for resume tests).
fn make_orchestrator_from_state(
    mock: MockAgentService,
    state: EpicState,
) -> (TestOrchestrator, Arc<MockAgentService>, EventLog) {
    let log = EventLog::new();
    let mock_arc = Arc::new(mock);
    let store = EpicStore::from_state(
        state,
        Arc::clone(&mock_arc),
        log.clone(),
        None,
        LimitsConfig::default(),
        None,
    );
    let orchestrator = cue::Orchestrator::new(store, log.clone());
    (orchestrator, mock_arc, log)
}

/// Build a `cue::Orchestrator` from a pre-populated `EpicState` with custom limits.
fn make_orchestrator_from_state_with_limits(
    mock: MockAgentService,
    state: EpicState,
    limits: LimitsConfig,
) -> (TestOrchestrator, Arc<MockAgentService>, EventLog) {
    let log = EventLog::new();
    let mock_arc = Arc::new(mock);
    let store = EpicStore::from_state(
        state,
        Arc::clone(&mock_arc),
        log.clone(),
        None,
        limits,
        None,
    );
    let orchestrator = cue::Orchestrator::new(store, log.clone());
    (orchestrator, mock_arc, log)
}

/// Extract the final `EpicState` from the orchestrator after run.
fn into_state(orch: TestOrchestrator) -> EpicState {
    orch.into_store().into_state()
}

/// Root(branch) -> one child(leaf) -> success -> verification pass -> Completed.
#[tokio::test]
async fn single_leaf() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // leaf child verification
        .file_review_pass() // leaf child file review
        .branch_verify_pass() // root branch verification (3 phases)
        .build();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
    mb.verify_passes(2) // leaf children verification
        .file_review_passes(2) // leaf children file review
        .branch_verify_pass(); // root branch verification
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    assert_eq!(state.get(root_id).unwrap().subtask_ids.len(), 2);
}

/// State is checkpointed to disk during execution.
#[tokio::test]
async fn checkpoint_saves_state() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
        .build();

    let dir = std::env::temp_dir().join("epic_test_checkpoint_cue");
    std::fs::create_dir_all(&dir).unwrap();
    let state_path = dir.join("state.json");
    let _ = std::fs::remove_file(&state_path);

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let mut orch = orch.with_state_path(state_path.clone());
    let result = orch.run(root_id).await.unwrap();
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .verify_pass() // grandchild leaf
        .file_review_pass() // grandchild leaf file review
        .branch_verify_pass() // mid branch
        .branch_verify_pass() // root branch
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let mid = state.get(mid_id).unwrap();
    assert_eq!(mid.path, Some(TaskPath::Branch));
    assert_eq!(mid.subtask_ids, vec![grandchild_id]);
    assert_eq!(mid.phase, TaskPhase::Completed);
}

/// Resume: task in Verifying phase goes straight to re-verification, not re-execution.
#[tokio::test]
async fn resume_verifying_skips_execution() {
    let mock = MockBuilder::new()
        .verify_pass() // child leaf (in Verifying phase)
        .file_review_pass() // child leaf file review
        .branch_verify_pass() // root branch
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    assert_eq!(state.get(child_id).unwrap().phase, TaskPhase::Completed);
    assert!(state.get(child_id).unwrap().attempts.is_empty());
}

/// Custom `max_depth`: child at depth limit is forced to Leaf without assess.
#[tokio::test]
async fn custom_max_depth_forces_leaf() {
    let mock = MockBuilder::new()
        .decompose_one()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
        .build();

    let limits = LimitsConfig {
        max_depth: 2,
        ..LimitsConfig::default()
    };

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let root = Task::new(root_id, None, "deep root".into(), vec!["passes".into()], 1);
    state.insert(root);

    let (orch, _mock_arc, _log) =
        make_orchestrator_from_state_with_limits(mock, state, limits.clone());
    let mut orch = orch.with_limits(limits);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
    mb.verify_passes(2) // two leaf children
        .file_review_passes(2) // two leaf file reviews
        .branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let child_a_id = state.get(root_id).unwrap().subtask_ids[0];
    let child_a = state.get(child_a_id).unwrap();
    assert_eq!(
        child_a.discoveries,
        vec!["API uses v2 format", "cache layer found"]
    );

    let mut found_discoveries_event = false;
    for event in log.snapshot() {
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
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // original leaf child
        .file_review_pass() // original leaf file review
        .branch_correctness_fail("root check failed") // root branch fails correctness
        .fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // fix leaf child
        .file_review_pass() // fix leaf file review
        .branch_verify_pass(); // root branch re-verify passes
    let mock = mb.build();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // grandchild leaf
    mb.branch_correctness_fail("mid check failed"); // mid branch fails

    for _ in 0..3 {
        mb.fix_subtask_one()
            .assess_leaf()
            .leaf_success()
            .verify_pass() // fix leaf child
            .file_review_pass() // fix leaf file review
            .branch_correctness_fail("still failing"); // mid branch re-verify fails
    }
    mb.recovery_unrecoverable();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let state = into_state(orch);
    let mid_id = state.get(root_id).unwrap().subtask_ids[0];
    let mid = state.get(mid_id).unwrap();
    assert_eq!(mid.verification_fix_rounds, 3);
    assert_eq!(mid.phase, TaskPhase::Failed);
    assert_eq!(mid.subtask_ids.len(), 4);
}

/// Fix tasks (leaf or branch) fail immediately to prevent recursive fix-within-fix.
#[tokio::test]
async fn fix_subtasks_no_recursive_fix() {
    // --- Part 1: Leaf fix subtask that fails verification must NOT enter leaf fix loop ---
    let mut mb = MockBuilder::new();
    // Root branches, original child succeeds, root verify fails.
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // original leaf child
        .file_review_pass() // original leaf file review
        .branch_correctness_fail("root check failed"); // root branch fails
    // Fix round 1: leaf fix subtask succeeds but verification fails.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_fail("fix leaf failed"); // fix leaf verification fails (is_fix_task)
    mb.branch_correctness_fail("root still failing"); // root branch still fails
    // Fix round 2: simple fix subtask succeeds and verification passes.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // fix leaf child
        .file_review_pass() // fix leaf file review
        .branch_verify_pass(); // root branch re-verify passes
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .verify_pass() // original leaf child
        .file_review_pass() // original leaf file review
        .branch_correctness_fail("root check failed"); // root branch fails
    // Fix round 1: branch fix subtask.
    mb.fix_subtask_one().assess_branch().decompose_one();
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // grandchild leaf
    mb.branch_correctness_fail("branch fix subtask failed"); // branch fix subtask fails (is_fix_task -> FailedNoFixLoop)
    mb.branch_correctness_fail("root still failing"); // root branch still fails
    // Fix round 2: simple fix subtask succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // fix leaf child
        .file_review_pass() // fix leaf file review
        .branch_verify_pass(); // root branch re-verify passes
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .verify_fail("still broken") // child leaf verify fails
        .fix_leaf_success() // fix succeeds
        .verify_pass() // fix verify passes
        .file_review_pass() // fix file review
        .branch_verify_pass() // root branch
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
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

    let (mut orch, _mock_arc, log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.fix_attempts.len(), 4);
    assert_eq!(child.fix_attempts[3].model, Model::Sonnet);
    assert!(child.fix_attempts[3].succeeded);

    let mut saw_escalation = false;
    for event in log.snapshot() {
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
        .verify_pass() // original leaf child
        .file_review_pass() // original leaf file review
        .branch_correctness_fail("root check failed"); // root branch fails

    for round in 1..=4 {
        mb.fix_subtask_one()
            .assess_leaf()
            .leaf_success()
            .verify_pass() // fix leaf child
            .file_review_pass(); // fix leaf file review
        if round < 4 {
            mb.branch_correctness_fail("root still failing"); // root branch re-verify fails
        } else {
            mb.branch_verify_pass(); // root branch re-verify passes
        }
    }
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 4);
    assert_eq!(root.phase, TaskPhase::Completed);
    assert_eq!(root.subtask_ids.len(), 5);

    let mut branch_fix_rounds: Vec<(u32, Model)> = Vec::new();
    for event in log.snapshot() {
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // recovery child leaf
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // child B leaf
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
#[tokio::test]
async fn recovery_full_redecomp_preserves_completed_siblings() {
    let mut mb = MockBuilder::new();
    mb.decompose_three();
    // Child A: succeeds.
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass();
    // Child B: fails terminally.
    mb.assess_leaf().leaf_failures(9, "B failed");
    // Recovery: full re-decomposition.
    mb.recovery_recoverable("redo").recovery_plan_full();
    // Recovery subtask: succeeds.
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass();
    // Root verification passes.
    mb.branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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

    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted"))
    );
    let state = into_state(orch);
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 2);
}

/// Fix tasks do not attempt recovery (prevents recursive recovery chains).
/// Tested via full orchestration: a fix-task parent with a failing child should
/// propagate failure without entering recovery.
#[tokio::test]
async fn recovery_not_attempted_for_fix_tasks() {
    let mut mb = MockBuilder::new();
    // The root is a fix task (branch). It decomposes into one child that fails.
    mb.decompose_one();
    mb.assess_leaf().leaf_failures(9, "child broke");

    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let mut root = Task::new(root_id, None, "fix parent".into(), vec!["passes".into()], 0);
    root.is_fix_task = true;
    state.insert(root);

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mb.build(), state);
    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let state = into_state(orch);
    // Recovery rounds should be 0 -- recovery was never attempted.
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 0);
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
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));
    let state = into_state(orch);
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // recovery child leaf
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { reason } if reason.contains("no subtasks")));
    let state = into_state(orch);
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);
}

/// Recovery events are emitted correctly.
#[tokio::test]
async fn recovery_emits_events() {
    let mut mb = MockBuilder::new();
    mb.decompose_one().assess_leaf().leaf_failures(9, "broke");
    mb.recovery_recoverable("fix it")
        .recovery_plan_incremental();
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // recovery child leaf
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let mut saw_recovery_started = false;
    let mut saw_recovery_plan = false;
    let mut saw_recovery_subtasks = false;
    for event in log.snapshot() {
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
    mb.verify_passes(2).file_review_passes(2); // two leaf children
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(
        root.checkpoint_guidance.as_deref(),
        Some("switch to API v2 format")
    );

    let mut saw_adjust = false;
    for event in log.snapshot() {
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
    mb.verify_pass().file_review_pass(); // child A leaf verification
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // recovery child leaf
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // child B leaf
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);

    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    for event in log.snapshot() {
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
    mb.verify_pass().file_review_pass(); // child A leaf
    mb.checkpoint_escalate();
    mb.recovery_unrecoverable();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
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
    mb.verify_pass().file_review_pass(); // child leaf
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    for event in log.snapshot() {
        assert!(
            !matches!(event, Event::CheckpointAdjust { .. }),
            "unexpected CheckpointAdjust event after agent error"
        );
        assert!(
            !matches!(event, Event::CheckpointEscalate { .. }),
            "unexpected CheckpointEscalate event after agent error"
        );
    }

    let state = into_state(orch);
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
    mb.verify_passes(2).file_review_passes(2); // two leaf children
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
    mb.verify_passes(3).file_review_passes(3); // three leaf children
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    assert_eq!(
        state.get(root_id).unwrap().checkpoint_guidance.as_deref(),
        Some("use API v2\nalso use gzip")
    );

    let mut adjust_count = 0;
    for event in log.snapshot() {
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
    mb.verify_pass().file_review_pass(); // child A leaf
    mb.checkpoint_escalate();

    // Build state with root marked as fix task.
    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let mut root = Task::new(
        root_id,
        None,
        "root goal".into(),
        vec!["root passes".into()],
        0,
    );
    root.is_fix_task = true;
    state.insert(root);

    let (mut orch, _mock_arc, log) = make_orchestrator_from_state(mb.build(), state);
    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    for event in log.snapshot() {
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

/// Checkpoint guidance flows to child context via `EpicStore::build_tree_context`.
#[test]
fn checkpoint_guidance_flows_to_child_context() {
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

    let mock = MockAgentService::new();
    let log = EventLog::new();
    let mock_arc = Arc::new(mock);
    let store = EpicStore::from_state(state, mock_arc, log, None, LimitsConfig::default(), None);

    let tree_ctx = cue::TaskStore::build_tree_context(&store, second_child_id).unwrap();
    assert_eq!(
        tree_ctx.checkpoint_guidance.as_deref(),
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
    mb.verify_pass().file_review_pass(); // child A leaf
    mb.checkpoint_escalate();

    // Build state with pre-set recovery_rounds.
    let mut state = EpicState::new();
    let root_id = state.next_task_id();
    let mut root = Task::new(
        root_id,
        None,
        "root goal".into(),
        vec!["root passes".into()],
        0,
    );
    root.recovery_rounds = 2;
    state.insert(root);

    let (mut orch, _mock_arc, log) = make_orchestrator_from_state(mb.build(), state);
    let result = orch.run(root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("recovery rounds exhausted")),
        "expected failure with 'recovery rounds exhausted', got: {result:?}"
    );

    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    for event in log.snapshot() {
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
    mb.verify_pass().file_review_pass(); // child A leaf
    mb.checkpoint_adjust("old guidance");
    mb.assess_leaf();
    mb.leaf_success_with_discoveries(vec!["approach is fundamentally wrong".into()]);
    mb.verify_pass().file_review_pass(); // child B leaf
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // recovery child leaf
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // child C leaf
    mb.branch_verify_pass(); // root branch
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    assert!(
        state.get(root_id).unwrap().checkpoint_guidance.is_none(),
        "checkpoint_guidance should be None after escalation clears prior adjust guidance"
    );

    let mut saw_adjust = false;
    let mut saw_escalate = false;
    let mut saw_recovery_started = false;
    for event in log.snapshot() {
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
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

    let (mut orch, _mock_arc, log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.attempts.len(), 4);
    assert_eq!(child.attempts[2].model, Model::Haiku);
    assert!(!child.attempts[2].succeeded);
    assert_eq!(child.attempts[3].model, Model::Sonnet);
    assert!(child.attempts[3].succeeded);

    let mut saw_escalation = false;
    for event in log.snapshot() {
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
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

    let (mut orch, _mock_arc, log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.attempts.len(), 7);
    assert_eq!(child.attempts[5].model, Model::Sonnet);
    assert!(!child.attempts[5].succeeded);
    assert_eq!(child.attempts[6].model, Model::Opus);
    assert!(child.attempts[6].succeeded);

    let mut saw_escalation = false;
    for event in log.snapshot() {
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
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

    let (mut orch, _mock_arc, log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let child = state.get(child_id).unwrap();
    assert_eq!(child.phase, TaskPhase::Completed);
    assert_eq!(child.attempts.len(), 4);
    assert_eq!(child.attempts[3].model, Model::Sonnet);
    assert!(child.attempts[3].succeeded);

    let mut saw_escalation = false;
    for event in log.snapshot() {
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
        .build();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let state_path = tmp.path().to_path_buf();

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let mut orch = orch.with_state_path(state_path.clone());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let child_id = state.get(root_id).unwrap().subtask_ids[0];

    let loaded_state = EpicState::load(&state_path).unwrap();
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

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);

    let result = orch.run(root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { reason } if reason.contains("recovery rounds exhausted"))
    );
    let state = into_state(orch);
    assert_eq!(state.get(root_id).unwrap().recovery_rounds, 1);
}

/// Custom `root_fix_rounds`=1: root verification fails -> 1 fix round -> still fails -> task fails.
#[tokio::test]
async fn custom_root_fix_rounds_limits_fix_attempts() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // original leaf child
        .file_review_pass() // original leaf file review
        .branch_correctness_fail("root check failed"); // root branch fails
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass() // fix leaf child
        .file_review_pass(); // fix leaf file review
    mb.branch_correctness_fail("root still failing"); // root branch re-verify fails

    let limits = LimitsConfig {
        root_fix_rounds: 1,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);

    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let state = into_state(orch);
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // grandchild leaf
    mb.branch_correctness_fail("branch check failed"); // mid branch fails
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // fix leaf child
    mb.branch_correctness_fail("branch still failing"); // mid branch re-verify fails
    mb.recovery_unrecoverable();

    let limits = LimitsConfig {
        branch_fix_rounds: 1,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);

    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let state = into_state(orch);
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
        .build();

    let limits = LimitsConfig {
        retry_budget: 0,
        ..LimitsConfig::default()
    };

    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator_with_limits(mock, limits);

    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
        .build();
    let (mut orch, mock_arc, root_id, _log) = make_orchestrator(mock);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let captured = mock_arc.decompose_models.lock().unwrap().clone();
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

    let limits = LimitsConfig {
        max_total_tasks: 2,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);
    let result = orch.run(root_id).await.unwrap();
    let TaskOutcome::Failed { reason } = &result else {
        panic!("expected TaskOutcome::Failed, got {result:?}");
    };
    assert!(
        reason.contains("task limit reached"),
        "unexpected reason: {reason}"
    );

    let mut limit_events: Vec<TaskId> = Vec::new();
    for event in log.snapshot() {
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
        .verify_pass()
        .file_review_pass(); // leaf child
    mb.branch_correctness_fail("root verification failed"); // root branch fails
    mb.fix_subtask_one();

    let limits = LimitsConfig {
        max_total_tasks: 2,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);
    let result = orch.run(root_id).await.unwrap();
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

    let limits = LimitsConfig {
        max_total_tasks: 2,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);
    let result = orch.run(root_id).await.unwrap();
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
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass(); // recovery child leaf
    mb.branch_verify_pass(); // root branch

    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);

    let result = orch.run(root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("recovery rounds exhausted")),
        "expected recovery-exhausted failure, got {result:?}"
    );

    let state = into_state(orch);
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
        .verify_pass()
        .branch_verify_pass()
        .build();

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let mut orch = orch.with_limits(LimitsConfig {
        max_total_tasks: 0,
        ..LimitsConfig::default()
    });
    let result = orch.run(root_id).await.unwrap();
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
    mb.verify_passes(2)
        .file_review_passes(2) // two leaf children
        .branch_verify_pass(); // root branch

    let limits = LimitsConfig {
        max_total_tasks: 3,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);
    let state = into_state(orch);
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
        .file_review_pass()
        .branch_correctness_fail("root check failed");
    mb.fix_subtask_error(TaskId(0), "LLM timeout");
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.phase, TaskPhase::Completed);
    assert_eq!(root.verification_fix_rounds, 2);
}

/// Branch fix loop: branch verify fails on round 1 re-verification, passes on round 2.
#[tokio::test]
async fn branch_fix_verify_error_retries() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_correctness_fail("root check failed");
    // Round 1: fix subtask succeeds, root re-verify fails completeness.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass();
    mb.branch_correctness_pass()
        .branch_completeness_fail("transient verify failure");
    // Round 2: fix subtask succeeds, root re-verify passes.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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
        .file_review_pass()
        .branch_correctness_fail("root check failed");
    mb.fix_subtask_errors(
        TaskId(0),
        vec![
            Some("LLM timeout round 1".into()),
            Some("LLM timeout round 2".into()),
        ],
    );
    mb.recovery_unrecoverable();

    let limits = LimitsConfig {
        root_fix_rounds: 2,
        ..LimitsConfig::default()
    };

    let (orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let mut orch = orch.with_limits(limits);
    let result = orch.run(root_id).await.unwrap();
    assert!(matches!(result, TaskOutcome::Failed { .. }));

    let state = into_state(orch);
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
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let result = orch.run(root_id).await;
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
    assert!(
        matches!(result, TaskOutcome::Failed { ref reason } if reason.contains("all non-fix children failed")),
        "expected Failure when all children failed, got: {result:?}"
    );
}

/// Branch with a mix of failed and completed non-fix children should still report Success.
#[tokio::test]
async fn branch_succeeds_when_some_children_completed() {
    let mock = MockBuilder::new().branch_verify_pass().build();
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

    let (mut orch, _mock_arc, _log) = make_orchestrator_from_state(mock, state);
    let result = orch.run(root_id).await.unwrap();
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
        .verify_pass() // leaf child verification
        .file_review_pass() // leaf child file review
        .branch_verify_pass() // root branch verification
        .build();
    let (mut orch, _mock_arc, root_id, log) = make_orchestrator(mock);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let mut root_review_events = 0;
    for event in log.snapshot() {
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
        .file_review_pass() // original leaf child
        .branch_correctness_fail("root check failed"); // root branch fails
    // Fix round 1: fix subtask passes verification but fails file review.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_fail("fix incomplete"); // fix leaf fails file review (is_fix_task)
    mb.branch_correctness_fail("root still failing"); // root branch still fails
    // Fix round 2: succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass() // fix leaf succeeds
        .branch_verify_pass(); // root branch re-verify passes
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
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

// -----------------------------------------------------------------------
// Three-phase branch verification tests
// -----------------------------------------------------------------------

/// All three branch review phases pass -> BranchVerifyOutcome::Passed.
#[tokio::test]
async fn branch_verify_all_three_phases_pass() {
    let mock = MockBuilder::new()
        .decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass()
        .build();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mock);
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.phase, TaskPhase::Completed);
}

/// Correctness fails -> early return, no completeness/simplification calls consumed.
#[tokio::test]
async fn branch_verify_correctness_fails_early_return() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_correctness_fail("interface mismatch");
    // Fix round: succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 1);
}

/// Completeness fails -> no simplification call consumed.
#[tokio::test]
async fn branch_verify_completeness_fails_no_simplification() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_correctness_pass()
        .branch_completeness_fail("missing requirement X");
    // Fix round: succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 1);
}

/// Simplification fails -> BranchVerifyOutcome::Failed (triggers fix round).
#[tokio::test]
async fn branch_verify_simplification_fails() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_correctness_pass()
        .branch_completeness_pass()
        .branch_simplification_fail("redundant abstraction layer");
    // Fix round: succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 1);
}

/// Fix task: any branch verification failure -> FailedNoFixLoop.
#[tokio::test]
async fn branch_verify_fix_task_fails_no_fix_loop() {
    let mut mb = MockBuilder::new();
    mb.decompose_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_correctness_fail("root check failed");
    // Fix round 1: branch fix subtask that is itself a branch. Its branch verify fails.
    mb.fix_subtask_one().assess_branch().decompose_one();
    mb.assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass();
    mb.branch_correctness_fail("fix branch failed"); // is_fix_task -> FailedNoFixLoop
    // Root re-verifies after fix round 1 (fix subtask failed, root still re-checks).
    mb.branch_correctness_fail("root still failing");
    // Root fix round 2: succeeds.
    mb.fix_subtask_one()
        .assess_leaf()
        .leaf_success()
        .verify_pass()
        .file_review_pass()
        .branch_verify_pass();
    let (mut orch, _mock_arc, root_id, _log) = make_orchestrator(mb.build());
    let result = orch.run(root_id).await.unwrap();
    assert_eq!(result, TaskOutcome::Success);

    let state = into_state(orch);
    let root = state.get(root_id).unwrap();
    assert_eq!(root.verification_fix_rounds, 2);
    let fix1_id = root.subtask_ids[1];
    let fix1 = state.get(fix1_id).unwrap();
    assert!(fix1.is_fix_task);
    assert_eq!(fix1.phase, TaskPhase::Failed);
    assert_eq!(
        fix1.verification_fix_rounds, 0,
        "fix-task branch should not enter its own fix loop"
    );
}
