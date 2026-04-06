// Builds TaskContext from TreeContext + Task.

use crate::agent::TaskContext;
use crate::task::Task;

pub use cue::TreeContext;

/// Combine a tree snapshot with a task to produce a full `TaskContext`
/// for agent calls.
pub fn tree_to_task_context(tree: &TreeContext, task: &Task) -> TaskContext {
    TaskContext {
        task: task.clone(),
        parent_goal: tree.parent_goal.clone(),
        ancestor_goals: tree.ancestor_goals.clone(),
        completed_siblings: tree.completed_siblings.clone(),
        pending_sibling_goals: tree.pending_sibling_goals.clone(),
        checkpoint_guidance: tree.checkpoint_guidance.clone(),
        children: tree.children.clone(),
        parent_discoveries: tree.parent_discoveries.clone(),
        parent_decomposition_rationale: tree.parent_decomposition_rationale.clone(),
    }
}
