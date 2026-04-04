// Builds TreeContext (read-only tree snapshot) and TaskContext from EpicState.

use crate::agent::{ChildStatus, ChildSummary, SiblingSummary, TaskContext};
use crate::orchestrator::OrchestratorError;
use crate::state::EpicState;
use crate::task::{Task, TaskId, TaskOutcome, TaskPhase};

/// Read-only snapshot of tree state around a task. Built by the orchestrator
/// before calling a task method. Owned data, not references into state —
/// avoids borrow conflicts with `&mut Task`.
#[derive(Debug, Clone)]
pub struct TreeContext {
    pub parent_goal: Option<String>,
    pub parent_decomposition_rationale: Option<String>,
    pub parent_discoveries: Vec<String>,
    pub ancestor_goals: Vec<String>,
    pub completed_siblings: Vec<SiblingSummary>,
    pub pending_sibling_goals: Vec<String>,
    pub children: Vec<ChildSummary>,
    pub checkpoint_guidance: Option<String>,
}

impl TreeContext {
    /// Combine this tree snapshot with a task to produce a full `TaskContext`
    /// for agent calls.
    pub fn to_task_context(&self, task: &Task) -> TaskContext {
        TaskContext {
            task: task.clone(),
            parent_goal: self.parent_goal.clone(),
            ancestor_goals: self.ancestor_goals.clone(),
            completed_siblings: self.completed_siblings.clone(),
            pending_sibling_goals: self.pending_sibling_goals.clone(),
            checkpoint_guidance: self.checkpoint_guidance.clone(),
            children: self.children.clone(),
            parent_discoveries: self.parent_discoveries.clone(),
            parent_decomposition_rationale: self.parent_decomposition_rationale.clone(),
        }
    }
}

/// Build a [`TreeContext`] (tree snapshot without the task itself).
#[allow(clippy::too_many_lines)]
pub fn build_tree_context(state: &EpicState, id: TaskId) -> Result<TreeContext, OrchestratorError> {
    let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;

    let parent = task.parent_id.and_then(|pid| state.get(pid));

    let parent_goal = parent.map(|p| p.goal.clone());

    let mut ancestor_goals = Vec::new();
    let mut cursor = task.parent_id;
    while let Some(pid) = cursor {
        if let Some(p) = state.get(pid) {
            ancestor_goals.push(p.goal.clone());
            cursor = p.parent_id;
        } else {
            break;
        }
    }

    let (completed_siblings, pending_sibling_goals) = parent.map_or_else(
        || (Vec::new(), Vec::new()),
        |parent| {
            let mut completed = Vec::new();
            let mut pending = Vec::new();
            for &sib_id in &parent.subtask_ids {
                if sib_id == id {
                    continue;
                }
                let Some(sib) = state.get(sib_id) else {
                    continue;
                };
                match sib.phase {
                    TaskPhase::Completed => {
                        completed.push(SiblingSummary {
                            id: sib_id,
                            goal: sib.goal.clone(),
                            outcome: TaskOutcome::Success,
                            discoveries: sib.discoveries.clone(),
                        });
                    }
                    TaskPhase::Failed => {
                        let reason = sib
                            .attempts
                            .iter()
                            .rev()
                            .find_map(|a| a.error.clone())
                            .unwrap_or_else(|| "unknown".into());
                        completed.push(SiblingSummary {
                            id: sib_id,
                            goal: sib.goal.clone(),
                            outcome: TaskOutcome::Failed { reason },
                            discoveries: sib.discoveries.clone(),
                        });
                    }
                    _ => {
                        pending.push(sib.goal.clone());
                    }
                }
            }
            (completed, pending)
        },
    );

    let checkpoint_guidance = parent.and_then(|p| p.checkpoint_guidance.clone());

    let children = task
        .subtask_ids
        .iter()
        .filter_map(|&cid| {
            let child = state.get(cid)?;
            let status = match child.phase {
                TaskPhase::Completed => ChildStatus::Completed,
                TaskPhase::Failed => {
                    let reason = child
                        .attempts
                        .iter()
                        .rev()
                        .find_map(|a| a.error.clone())
                        .unwrap_or_else(|| "unknown".into());
                    ChildStatus::Failed { reason }
                }
                TaskPhase::Pending => ChildStatus::Pending,
                _ => ChildStatus::InProgress,
            };
            Some(ChildSummary {
                goal: child.goal.clone(),
                status,
                discoveries: child.discoveries.clone(),
            })
        })
        .collect();

    let parent_discoveries = parent.map_or_else(Vec::new, |p| p.discoveries.clone());
    let parent_decomposition_rationale = parent.and_then(|p| p.decomposition_rationale.clone());

    Ok(TreeContext {
        parent_goal,
        parent_decomposition_rationale,
        parent_discoveries,
        ancestor_goals,
        completed_siblings,
        pending_sibling_goals,
        children,
        checkpoint_guidance,
    })
}

/// Build a full [`TaskContext`] (tree snapshot + task clone).
pub fn build_context(state: &EpicState, id: TaskId) -> Result<TaskContext, OrchestratorError> {
    let tree = build_tree_context(state, id)?;
    let task = state.get(id).ok_or(OrchestratorError::TaskNotFound(id))?;
    Ok(tree.to_task_context(task))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Attempt, Model};

    /// `build_context` populates `parent_decomposition_rationale`, `parent_discoveries`, and `children`.
    #[test]
    fn populates_parent_fields_and_children() {
        let mut state = EpicState::new();
        let parent_id = state.next_task_id(); // T0
        let child_id = state.next_task_id(); // T1

        let mut parent = Task::new(
            parent_id,
            None,
            "parent goal".into(),
            vec!["parent passes".into()],
            0,
        );
        parent.decomposition_rationale = Some("split by module".into());
        parent.discoveries = vec!["API uses v2".into(), "config moved".into()];
        parent.subtask_ids = vec![child_id];

        let mut child = Task::new(
            child_id,
            Some(parent_id),
            "child goal".into(),
            vec!["child passes".into()],
            1,
        );
        child.phase = TaskPhase::Completed;
        child.discoveries = vec!["found bug".into()];

        state.insert(parent);
        state.insert(child);

        // Build context for the child — should pull parent fields.
        let ctx = build_context(&state, child_id).unwrap();
        assert_eq!(
            ctx.parent_decomposition_rationale.as_deref(),
            Some("split by module"),
        );
        assert_eq!(ctx.parent_discoveries, vec!["API uses v2", "config moved"]);

        // Build context for the parent — should have children populated.
        let parent_ctx = build_context(&state, parent_id).unwrap();
        assert_eq!(parent_ctx.children.len(), 1);
        assert_eq!(parent_ctx.children[0].goal, "child goal");
        assert!(matches!(
            parent_ctx.children[0].status,
            ChildStatus::Completed
        ));
    }

    /// `build_context` maps all `TaskPhase` variants to correct `ChildStatus`.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn child_status_mapping_all_phases() {
        let mut state = EpicState::new();
        let parent_id = state.next_task_id(); // T0
        let completed_id = state.next_task_id(); // T1
        let failed_id = state.next_task_id(); // T2
        let pending_id = state.next_task_id(); // T3
        let executing_id = state.next_task_id(); // T4
        let assessing_id = state.next_task_id(); // T5
        let verifying_id = state.next_task_id(); // T6

        let mut parent = Task::new(parent_id, None, "parent".into(), vec!["passes".into()], 0);
        parent.subtask_ids = vec![
            completed_id,
            failed_id,
            pending_id,
            executing_id,
            assessing_id,
            verifying_id,
        ];

        let mut completed_child = Task::new(
            completed_id,
            Some(parent_id),
            "completed child".into(),
            vec!["done".into()],
            1,
        );
        completed_child.phase = TaskPhase::Completed;

        let mut failed_child = Task::new(
            failed_id,
            Some(parent_id),
            "failed child".into(),
            vec!["done".into()],
            1,
        );
        failed_child.phase = TaskPhase::Failed;
        failed_child.attempts.push(Attempt {
            model: Model::Haiku,
            succeeded: false,
            error: Some("compile error".into()),
        });

        let pending_child = Task::new(
            pending_id,
            Some(parent_id),
            "pending child".into(),
            vec!["done".into()],
            1,
        );

        let mut executing_child = Task::new(
            executing_id,
            Some(parent_id),
            "executing child".into(),
            vec!["done".into()],
            1,
        );
        executing_child.phase = TaskPhase::Executing;

        let mut assessing_child = Task::new(
            assessing_id,
            Some(parent_id),
            "assessing child".into(),
            vec!["done".into()],
            1,
        );
        assessing_child.phase = TaskPhase::Assessing;

        let mut verifying_child = Task::new(
            verifying_id,
            Some(parent_id),
            "verifying child".into(),
            vec!["done".into()],
            1,
        );
        verifying_child.phase = TaskPhase::Verifying;

        state.insert(parent);
        state.insert(completed_child);
        state.insert(failed_child);
        state.insert(pending_child);
        state.insert(executing_child);
        state.insert(assessing_child);
        state.insert(verifying_child);

        let ctx = build_context(&state, parent_id).unwrap();
        assert_eq!(ctx.children.len(), 6);

        assert!(
            matches!(ctx.children[0].status, ChildStatus::Completed),
            "Completed phase should map to ChildStatus::Completed"
        );
        match &ctx.children[1].status {
            ChildStatus::Failed { reason } => {
                assert_eq!(reason, "compile error");
            }
            other => panic!("Failed phase should map to ChildStatus::Failed, got {other:?}"),
        }
        assert!(
            matches!(ctx.children[2].status, ChildStatus::Pending),
            "Pending phase should map to ChildStatus::Pending"
        );
        assert!(
            matches!(ctx.children[3].status, ChildStatus::InProgress),
            "Executing phase should map to ChildStatus::InProgress"
        );
        assert!(
            matches!(ctx.children[4].status, ChildStatus::InProgress),
            "Assessing phase should map to ChildStatus::InProgress"
        );
        assert!(
            matches!(ctx.children[5].status, ChildStatus::InProgress),
            "Verifying phase should map to ChildStatus::InProgress"
        );
    }

    /// `build_context` silently skips subtask IDs that don't exist in state.
    #[test]
    fn skips_dangling_subtask_id() {
        let mut state = EpicState::new();
        let parent_id = state.next_task_id(); // T0
        let real_child_id = state.next_task_id(); // T1
        let dangling_id = state.next_task_id(); // T2 — never inserted

        let mut parent = Task::new(parent_id, None, "parent".into(), vec!["passes".into()], 0);
        parent.subtask_ids = vec![real_child_id, dangling_id];

        let real_child = Task::new(
            real_child_id,
            Some(parent_id),
            "real child".into(),
            vec!["child passes".into()],
            1,
        );

        state.insert(parent);
        state.insert(real_child);
        // dangling_id is NOT inserted

        let ctx = build_context(&state, parent_id).unwrap();
        assert_eq!(ctx.children.len(), 1, "should skip dangling subtask ID");
        assert_eq!(ctx.children[0].goal, "real child");
    }
}
