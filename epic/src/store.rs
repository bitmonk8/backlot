// EpicStore: TaskStore implementation wrapping EpicState + runtime deps.
// Bridges epic's persistence layer to cue's generic orchestrator.

use crate::agent::AgentService;
use crate::config::project::LimitsConfig;
use crate::events::EventLog;
use crate::state::EpicState;
use crate::task::node_impl::EpicTask;
use crate::task::{Task, TaskId, TaskRuntime};
use cue::OrchestratorError;
use cue::context::TreeContext;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Combines serializable task state with non-serializable runtime deps.
/// Implements `cue::TaskStore` so it can be used with `cue::Orchestrator`.
pub struct EpicStore<A: AgentService> {
    tasks: HashMap<TaskId, EpicTask<A>>,
    next_id: u64,
    root_id: Option<TaskId>,
    runtime: Option<Arc<TaskRuntime<A>>>,
}

impl<A: AgentService + 'static> EpicStore<A> {
    /// Create from an existing `EpicState` and runtime deps.
    pub fn from_state(
        state: EpicState,
        agent: Arc<A>,
        events: EventLog,
        vault: Option<Arc<vault::Vault>>,
        limits: LimitsConfig,
        project_root: Option<PathBuf>,
    ) -> Self {
        let runtime = Arc::new(TaskRuntime {
            agent,
            events,
            vault,
            limits,
            project_root,
        });
        let (tasks_map, next_id, root_id) = state.into_parts();
        let mut tasks = HashMap::new();
        for (id, task) in tasks_map {
            tasks.insert(id, EpicTask::new(task, Some(Arc::clone(&runtime))));
        }
        Self {
            tasks,
            next_id,
            root_id,
            runtime: Some(runtime),
        }
    }

    /// Extract the inner `EpicState` for persistence or status display.
    pub fn into_state(self) -> EpicState {
        let tasks: HashMap<TaskId, Task> = self
            .tasks
            .into_iter()
            .map(|(id, epic_task)| (id, epic_task.task))
            .collect();
        EpicState::from_parts(tasks, self.next_id, self.root_id)
    }

    /// Access inner state for read-only queries (status, usage).
    pub fn as_state(&self) -> EpicState {
        let tasks: HashMap<TaskId, Task> = self
            .tasks
            .iter()
            .map(|(id, epic_task)| (*id, epic_task.task.clone()))
            .collect();
        EpicState::from_parts(tasks, self.next_id, self.root_id)
    }

    const fn next_task_id(&mut self) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        id
    }

    fn collect_ancestor_goals(&self, mut cursor: Option<TaskId>) -> Vec<String> {
        let mut goals = Vec::new();
        while let Some(pid) = cursor {
            if let Some(p) = self.tasks.get(&pid) {
                goals.push(p.task.goal.clone());
                cursor = p.task.parent_id;
            } else {
                break;
            }
        }
        goals
    }

    fn collect_siblings(
        &self,
        parent: Option<&EpicTask<A>>,
        self_id: TaskId,
    ) -> (Vec<cue::SiblingSummary>, Vec<String>) {
        let Some(parent_task) = parent else {
            return (Vec::new(), Vec::new());
        };
        let mut completed = Vec::new();
        let mut pending = Vec::new();
        for &sib_id in &parent_task.task.subtask_ids {
            if sib_id == self_id {
                continue;
            }
            let Some(sib) = self.tasks.get(&sib_id) else {
                continue;
            };
            match sib.task.phase {
                cue::TaskPhase::Completed => {
                    completed.push(cue::SiblingSummary {
                        id: sib_id,
                        goal: sib.task.goal.clone(),
                        outcome: cue::TaskOutcome::Success,
                        discoveries: sib.task.discoveries.clone(),
                    });
                }
                cue::TaskPhase::Failed => {
                    let reason = sib
                        .task
                        .attempts
                        .iter()
                        .rev()
                        .find_map(|a| a.error.clone())
                        .unwrap_or_else(|| "unknown".into());
                    completed.push(cue::SiblingSummary {
                        id: sib_id,
                        goal: sib.task.goal.clone(),
                        outcome: cue::TaskOutcome::Failed { reason },
                        discoveries: sib.task.discoveries.clone(),
                    });
                }
                _ => {
                    pending.push(sib.task.goal.clone());
                }
            }
        }
        (completed, pending)
    }

    fn collect_children(&self, task: &EpicTask<A>) -> Vec<cue::ChildSummary> {
        task.task
            .subtask_ids
            .iter()
            .filter_map(|&cid| {
                let child = self.tasks.get(&cid)?;
                let status = match child.task.phase {
                    cue::TaskPhase::Completed => cue::ChildStatus::Completed,
                    cue::TaskPhase::Failed => {
                        let reason = child
                            .task
                            .attempts
                            .iter()
                            .rev()
                            .find_map(|a| a.error.clone())
                            .unwrap_or_else(|| "unknown".into());
                        cue::ChildStatus::Failed { reason }
                    }
                    cue::TaskPhase::Pending => cue::ChildStatus::Pending,
                    _ => cue::ChildStatus::InProgress,
                };
                Some(cue::ChildSummary {
                    goal: child.task.goal.clone(),
                    status,
                    discoveries: child.task.discoveries.clone(),
                })
            })
            .collect()
    }
}

impl<A: AgentService + 'static> cue::TaskStore for EpicStore<A> {
    type Task = EpicTask<A>;

    fn get(&self, id: TaskId) -> Option<&Self::Task> {
        self.tasks.get(&id)
    }

    fn get_mut(&mut self, id: TaskId) -> Option<&mut Self::Task> {
        self.tasks.get_mut(&id)
    }

    fn task_count(&self) -> usize {
        self.tasks.len()
    }

    fn dfs_order(&self, root: TaskId) -> Vec<TaskId> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            result.push(id);
            if let Some(task) = self.tasks.get(&id) {
                for child_id in task.task.subtask_ids.iter().rev().copied() {
                    stack.push(child_id);
                }
            }
        }
        result
    }

    fn set_root_id(&mut self, id: TaskId) {
        self.root_id = Some(id);
    }

    fn save(&self, path: &Path) -> anyhow::Result<()> {
        let state = self.as_state();
        state.save(path)
    }

    fn bind_runtime(&mut self) {
        let runtime = self.runtime.clone();
        for task in self.tasks.values_mut() {
            task.runtime.clone_from(&runtime);
        }
    }

    fn create_subtask(
        &mut self,
        parent_id: TaskId,
        spec: &cue::SubtaskSpec,
        mark_fix: bool,
        inherit_recovery_rounds: Option<u32>,
    ) -> TaskId {
        // Missing parent indicates a store-invariant violation: callers within this crate
        // always pass a parent_id that exists. Fail loudly rather than silently rooting the
        // child at depth 1 (see issue #134).
        let parent_depth = self
            .tasks
            .get(&parent_id)
            .unwrap_or_else(|| panic!("create_subtask: parent {parent_id:?} not found in store"))
            .task
            .depth;
        let child_id = self.next_task_id();
        let mut child = Task::new(
            child_id,
            Some(parent_id),
            spec.goal.clone(),
            spec.verification_criteria.clone(),
            parent_depth + 1,
        );
        child.magnitude_estimate = Some(spec.magnitude_estimate);
        child.is_fix_task = mark_fix;
        if let Some(rounds) = inherit_recovery_rounds {
            child.recovery_rounds = rounds;
        }
        let runtime = self.runtime.clone();
        self.tasks.insert(child_id, EpicTask::new(child, runtime));
        child_id
    }

    fn any_non_fix_child_succeeded(&self, parent_id: TaskId) -> bool {
        let Some(parent) = self.tasks.get(&parent_id) else {
            return false;
        };
        parent.task.subtask_ids.iter().any(|&cid| {
            self.tasks
                .get(&cid)
                .is_some_and(|c| !c.task.is_fix_task && c.task.phase == cue::TaskPhase::Completed)
        })
    }

    fn build_tree_context(&self, id: TaskId) -> Result<TreeContext, OrchestratorError> {
        let task = self
            .tasks
            .get(&id)
            .ok_or(OrchestratorError::TaskNotFound(id))?;
        let parent = task.task.parent_id.and_then(|pid| self.tasks.get(&pid));
        let parent_goal = parent.map(|p| p.task.goal.clone());

        let ancestor_goals = self.collect_ancestor_goals(task.task.parent_id);
        let (completed_siblings, pending_sibling_goals) = self.collect_siblings(parent, id);
        let children = self.collect_children(task);

        let checkpoint_guidance = parent.and_then(|p| p.task.checkpoint_guidance.clone());
        let parent_discoveries = parent.map_or_else(Vec::new, |p| p.task.discoveries.clone());
        let parent_decomposition_rationale =
            parent.and_then(|p| p.task.decomposition_rationale.clone());

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
}
