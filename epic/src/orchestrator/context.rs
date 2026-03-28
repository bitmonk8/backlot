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
