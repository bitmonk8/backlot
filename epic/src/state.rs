// EpicState: task tree persistence and session resume.

use crate::task::{Task, TaskId};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct EpicState {
    tasks: HashMap<TaskId, Task>,
    next_id: u64,
}

#[allow(dead_code)] // Methods used in tests and future main wiring.
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

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.tasks.get_mut(&id)
    }

    /// DFS-ordered list of task IDs starting from the given root.
    pub fn dfs_order(&self, root: TaskId) -> Vec<TaskId> {
        let mut result = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            result.push(id);
            if let Some(task) = self.tasks.get(&id) {
                // Push in reverse so leftmost child is visited first.
                for child_id in task.subtask_ids.iter().rev().copied() {
                    stack.push(child_id);
                }
            }
        }
        result
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
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

        let dir = std::env::temp_dir().join("epic_test_state");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        state.save(&path).unwrap();

        let loaded = EpicState::load(&path).unwrap();
        assert_eq!(loaded.next_id, 2);

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
}
