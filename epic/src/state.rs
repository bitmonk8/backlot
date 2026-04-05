// EpicState: task tree persistence and session resume.

use crate::task::{Task, TaskId, TaskPhase, TaskUsage};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EpicState {
    tasks: HashMap<TaskId, Task>,
    next_id: u64,
    root_id: Option<TaskId>,
}

impl EpicState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decompose into raw parts for wrapping in `EpicStore`.
    pub fn into_parts(self) -> (HashMap<TaskId, Task>, u64, Option<TaskId>) {
        (self.tasks, self.next_id, self.root_id)
    }

    /// Reconstruct from raw parts (inverse of `into_parts`).
    pub const fn from_parts(
        tasks: HashMap<TaskId, Task>,
        next_id: u64,
        root_id: Option<TaskId>,
    ) -> Self {
        Self {
            tasks,
            next_id,
            root_id,
        }
    }

    pub const fn next_task_id(&mut self) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn insert(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }

    #[allow(dead_code)] // Used by legacy orchestrator retained for test migration
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    #[allow(dead_code)] // Used by legacy orchestrator retained for test migration
    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.get_mut(&id)
    }

    /// DFS-ordered list of task IDs starting from the given root.
    /// Each ID appears at most once (cycles and shared children are deduplicated).
    pub fn dfs_order(&self, root: TaskId) -> Vec<TaskId> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            result.push(id);
            if let Some(task) = self.tasks.get(&id) {
                // Preserve declaration order in output (stack is LIFO).
                for child_id in task.subtask_ids.iter().rev().copied() {
                    stack.push(child_id);
                }
            }
        }
        result
    }

    pub const fn set_root_id(&mut self, id: TaskId) {
        self.root_id = Some(id);
    }

    pub const fn root_id(&self) -> Option<TaskId> {
        self.root_id
    }

    /// Create a single subtask under the given parent, returning the new task ID.
    #[allow(dead_code)] // Used by legacy orchestrator retained for test migration
    pub fn create_subtask(
        &mut self,
        parent_id: TaskId,
        spec: &crate::task::branch::SubtaskSpec,
        mark_fix: bool,
        inherit_recovery_rounds: Option<u32>,
    ) -> Option<TaskId> {
        let parent_depth = self.tasks.get(&parent_id)?.depth;
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
        self.tasks.insert(child_id, child);
        Some(child_id)
    }

    /// Build a tree-context snapshot for the given task, used by lifecycle methods.
    #[allow(dead_code)] // Used by legacy orchestrator retained for test migration
    pub fn build_tree_context(
        &self,
        id: TaskId,
    ) -> Result<crate::orchestrator::context::TreeContext, crate::orchestrator::OrchestratorError>
    {
        crate::orchestrator::context::build_tree_context(self, id)
    }

    /// Check if any non-fix child of the given parent completed successfully.
    #[allow(dead_code)] // Used by legacy orchestrator retained for test migration
    pub fn any_non_fix_child_succeeded(&self, parent_id: TaskId) -> bool {
        let Some(parent) = self.tasks.get(&parent_id) else {
            return false;
        };
        parent.subtask_ids.iter().any(|&cid| {
            self.tasks
                .get(&cid)
                .is_some_and(|c| !c.is_fix_task && c.phase == TaskPhase::Completed)
        })
    }

    pub fn total_usage(&self) -> TaskUsage {
        let mut total = TaskUsage::zero();
        for task in self.tasks.values() {
            total.input_tokens += task.usage.input_tokens;
            total.output_tokens += task.usage.output_tokens;
            total.cache_creation_input_tokens += task.usage.cache_creation_input_tokens;
            total.cache_read_input_tokens += task.usage.cache_read_input_tokens;
            total.cost_usd += task.usage.cost_usd;
            total.api_calls += task.usage.api_calls;
            total.total_tool_calls += task.usage.total_tool_calls;
            total.total_latency_ms += task.usage.total_latency_ms;
        }
        total
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json)?;
        if let Err(e) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let mut state: Self = serde_json::from_str(&json)?;

        // Guard against corrupted/hand-edited files where next_id is too low.
        let max_existing = state.tasks.keys().map(|id| id.0).max().unwrap_or(0);
        if let Some(min_next) = max_existing.checked_add(1) {
            if state.next_id < min_next {
                state.next_id = min_next;
            }
        } else {
            bail!("task ID space exhausted (max existing ID is u64::MAX)");
        }

        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{MagnitudeEstimate, TaskPhase};

    #[test]
    fn persistence_round_trip() {
        let mut state = EpicState::new();
        let root_id = state.next_task_id();
        let mut root = Task::new(root_id, None, "root goal".into(), vec!["passes".into()], 0);
        root.phase = TaskPhase::Completed;

        let child_id = state.next_task_id();
        let mut child = Task::new(
            child_id,
            Some(root_id),
            "child goal".into(),
            vec!["child passes".into()],
            1,
        );
        child.magnitude_estimate = Some(MagnitudeEstimate::Small);
        child.phase = TaskPhase::Completed;

        root.subtask_ids.push(child_id);
        state.insert(root);
        state.insert(child);
        state.set_root_id(root_id);

        let dir = std::env::temp_dir().join("epic_test_state");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        state.save(&path).unwrap();

        // Atomic write must not leave a .tmp file behind.
        assert!(!dir.join("state.json.tmp").exists());

        let loaded = EpicState::load(&path).unwrap();
        assert_eq!(loaded.next_id, 2);
        assert_eq!(loaded.root_id(), Some(root_id));

        let loaded_root = loaded.get(root_id).unwrap();
        assert_eq!(loaded_root.goal, "root goal");
        assert_eq!(loaded_root.subtask_ids, vec![child_id]);

        let loaded_child = loaded.get(child_id).unwrap();
        assert_eq!(loaded_child.parent_id, Some(root_id));
        assert_eq!(
            loaded_child.magnitude_estimate,
            Some(MagnitudeEstimate::Small)
        );

        // DFS order
        let order = loaded.dfs_order(root_id);
        assert_eq!(order, vec![root_id, child_id]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dfs_order_self_cycle() {
        let mut state = EpicState::new();
        let id = TaskId(0);
        let mut t = Task::new(id, None, "self-ref".into(), vec![], 0);
        t.subtask_ids.push(id);
        state.insert(t);
        let order = state.dfs_order(id);
        assert_eq!(order, vec![id]);
    }

    #[test]
    fn dfs_order_mutual_cycle() {
        let mut state = EpicState::new();
        let a = TaskId(0);
        let b = TaskId(1);
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        let mut tb = Task::new(b, Some(a), "b".into(), vec![], 1);
        tb.subtask_ids.push(a);
        state.insert(ta);
        state.insert(tb);
        let order = state.dfs_order(a);
        assert_eq!(order, vec![a, b]);
    }

    #[test]
    fn dfs_order_acyclic() {
        let mut state = EpicState::new();
        let a = TaskId(0);
        let b = TaskId(1);
        let c = TaskId(2);
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        ta.subtask_ids.push(c);
        state.insert(ta);
        state.insert(Task::new(b, Some(a), "b".into(), vec![], 1));
        state.insert(Task::new(c, Some(a), "c".into(), vec![], 1));
        let order = state.dfs_order(a);
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn dfs_order_diamond_deduplicates() {
        let mut state = EpicState::new();
        let (a, b, c, d) = (TaskId(0), TaskId(1), TaskId(2), TaskId(3));
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        ta.subtask_ids.push(c);
        let mut tb = Task::new(b, Some(a), "b".into(), vec![], 1);
        tb.subtask_ids.push(d);
        let mut tc = Task::new(c, Some(a), "c".into(), vec![], 1);
        tc.subtask_ids.push(d);
        state.insert(ta);
        state.insert(tb);
        state.insert(tc);
        state.insert(Task::new(d, Some(b), "d".into(), vec![], 2));
        let order = state.dfs_order(a);
        // D appears once despite being referenced by both B and C.
        assert_eq!(order, vec![a, b, d, c]);
    }

    #[test]
    fn dfs_order_three_node_cycle() {
        let mut state = EpicState::new();
        let (a, b, c) = (TaskId(0), TaskId(1), TaskId(2));
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        let mut tb = Task::new(b, Some(a), "b".into(), vec![], 1);
        tb.subtask_ids.push(c);
        let mut tc = Task::new(c, Some(b), "c".into(), vec![], 2);
        tc.subtask_ids.push(a);
        state.insert(ta);
        state.insert(tb);
        state.insert(tc);
        let order = state.dfs_order(a);
        assert_eq!(order, vec![a, b, c]);
    }

    #[test]
    fn dfs_order_leaf_only() {
        let mut state = EpicState::new();
        let id = TaskId(0);
        state.insert(Task::new(id, None, "leaf".into(), vec![], 0));
        assert_eq!(state.dfs_order(id), vec![id]);
    }

    #[test]
    fn dfs_order_missing_root() {
        let state = EpicState::new();
        let order = state.dfs_order(TaskId(99));
        // Nonexistent root still appears (no children to traverse).
        assert_eq!(order, vec![TaskId(99)]);
    }

    #[test]
    fn dfs_order_dangling_subtask() {
        let mut state = EpicState::new();
        let a = TaskId(0);
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(TaskId(99)); // not in state
        state.insert(ta);
        let order = state.dfs_order(a);
        // Dangling ref appears in output (no panic), but has no children.
        assert_eq!(order, vec![a, TaskId(99)]);
    }

    #[test]
    fn dfs_order_duplicate_in_subtask_ids() {
        let mut state = EpicState::new();
        let (a, b) = (TaskId(0), TaskId(1));
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        ta.subtask_ids.push(b); // duplicate
        state.insert(ta);
        state.insert(Task::new(b, Some(a), "b".into(), vec![], 1));
        let order = state.dfs_order(a);
        assert_eq!(order, vec![a, b]);
    }

    #[test]
    fn dfs_order_excludes_unreachable() {
        let mut state = EpicState::new();
        let (a, b, c) = (TaskId(0), TaskId(1), TaskId(2));
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        state.insert(ta);
        state.insert(Task::new(b, Some(a), "b".into(), vec![], 1));
        state.insert(Task::new(c, None, "unreachable".into(), vec![], 0));
        let order = state.dfs_order(a);
        assert_eq!(order, vec![a, b]);
    }

    #[test]
    fn dfs_order_wide_fanout() {
        let mut state = EpicState::new();
        let (a, b, c, d) = (TaskId(0), TaskId(1), TaskId(2), TaskId(3));
        let mut ta = Task::new(a, None, "a".into(), vec![], 0);
        ta.subtask_ids.push(b);
        ta.subtask_ids.push(c);
        ta.subtask_ids.push(d);
        state.insert(ta);
        state.insert(Task::new(b, Some(a), "b".into(), vec![], 1));
        state.insert(Task::new(c, Some(a), "c".into(), vec![], 1));
        state.insert(Task::new(d, Some(a), "d".into(), vec![], 1));
        let order = state.dfs_order(a);
        assert_eq!(order, vec![a, b, c, d]);
    }

    #[test]
    fn load_repairs_next_id_below_max_task_id() {
        let mut state = EpicState::new();
        let id_a = TaskId(5);
        let id_b = TaskId(10);
        state.insert(Task::new(id_a, None, "a".into(), vec![], 0));
        state.insert(Task::new(id_b, Some(id_a), "b".into(), vec![], 1));
        // Deliberately set next_id below the max existing ID.
        state.next_id = 3;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&path, json).unwrap();

        let loaded = EpicState::load(&path).unwrap();
        // next_id must be at least max(existing) + 1.
        assert!(
            loaded.next_id > id_b.0,
            "next_id {} should exceed max task id {}",
            loaded.next_id,
            id_b.0
        );
        assert_eq!(loaded.next_id, 11);
    }

    #[test]
    fn load_preserves_valid_next_id() {
        let mut state = EpicState::new();
        let id = TaskId(2);
        state.insert(Task::new(id, None, "x".into(), vec![], 0));
        state.next_id = 50; // already well above max

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&path, json).unwrap();

        let loaded = EpicState::load(&path).unwrap();
        assert_eq!(loaded.next_id, 50);
    }

    #[test]
    fn load_nonexistent_file_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("does_not_exist.json");
        let err = EpicState::load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("No such file")
                || msg.contains("cannot find")
                || msg.contains("not found"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn load_invalid_content_errors() {
        let cases = [
            ("empty.json", ""),
            ("bad.json", "not json at all {{{"),
            ("wrong_schema.json", r#"{"unrelated_field": 42}"#),
            (
                "wrong_types.json",
                r#"{"tasks": "not a map", "next_id": "abc", "root_id": 123}"#,
            ),
        ];
        for (filename, content) in cases {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join(filename);
            std::fs::write(&path, content).unwrap();
            assert!(
                EpicState::load(&path).is_err(),
                "expected error for {filename}"
            );
        }
    }

    #[test]
    fn load_rejects_max_task_id_overflow() {
        let mut state = EpicState::new();
        let id = TaskId(u64::MAX);
        state.insert(Task::new(id, None, "max".into(), vec![], 0));
        state.next_id = 0; // deliberately low

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&path, json).unwrap();

        let err = EpicState::load(&path).unwrap_err();
        assert!(
            err.to_string().contains("exhausted"),
            "expected 'exhausted' error, got: {err}"
        );
    }
}
