// EpicState: task tree persistence and session resume.

use crate::task::{Task, TaskId};
use anyhow::Result;
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

    pub const fn next_task_id(&mut self) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn insert(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

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
        let state = serde_json::from_str(&json)?;
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
    fn task_count_tracks_insertions() {
        let mut state = EpicState::new();
        assert_eq!(state.task_count(), 0);

        let t1 = Task::new(TaskId(1), None, "goal 1".into(), vec![], 0);
        state.insert(t1);
        assert_eq!(state.task_count(), 1);

        let t2 = Task::new(TaskId(2), None, "goal 2".into(), vec![], 0);
        state.insert(t2);
        assert_eq!(state.task_count(), 2);
    }
}
