// Task tree widget: hierarchical display with status indicators.

use crate::task::{TaskId, TaskPath, TaskPhase};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Widget};
use std::collections::HashMap;

#[allow(dead_code)] // Fields are structural; used when interactive controls are added.
pub struct TuiTask {
    pub id: TaskId,
    pub parent_id: Option<TaskId>,
    pub goal: String,
    pub depth: u32,
    pub phase: TaskPhase,
    pub path: Option<TaskPath>,
    pub subtask_ids: Vec<TaskId>,
    pub current: bool,
    pub cost_usd: f64,
}

const fn phase_indicator(phase: TaskPhase) -> &'static str {
    match phase {
        TaskPhase::Completed => "✓",
        TaskPhase::Failed => "✗",
        TaskPhase::Executing | TaskPhase::Assessing | TaskPhase::Verifying => "▸",
        TaskPhase::Pending => "·",
    }
}

fn phase_style(phase: TaskPhase) -> Style {
    match phase {
        TaskPhase::Completed => Style::default().fg(Color::Green),
        TaskPhase::Failed => Style::default().fg(Color::Red),
        TaskPhase::Executing | TaskPhase::Assessing | TaskPhase::Verifying => {
            Style::default().fg(Color::Yellow)
        }
        TaskPhase::Pending => Style::default().fg(Color::DarkGray),
    }
}

/// Build DFS-ordered lines for the task tree.
pub fn render_tree_lines(
    tasks: &HashMap<TaskId, TuiTask>,
    root_id: Option<TaskId>,
    current_task: Option<TaskId>,
    area_width: u16,
) -> Vec<Line<'static>> {
    let Some(root_id) = root_id else {
        return vec![Line::from("  (no tasks)")];
    };

    let mut lines = Vec::new();
    let mut stack: Vec<TaskId> = vec![root_id];

    while let Some(id) = stack.pop() {
        let Some(task) = tasks.get(&id) else {
            continue;
        };

        let indent = "  ".repeat(task.depth as usize);
        let indicator = phase_indicator(task.phase);
        let cursor = if current_task == Some(id) { " ←" } else { "" };

        // Truncate goal to fit in available width.
        let prefix_len = indent.len() + 2 + cursor.len(); // "X " + indicator
        let max_goal = (area_width as usize).saturating_sub(prefix_len + 1);
        let goal_display = if max_goal == 0 {
            String::new()
        } else if task.goal.len() > max_goal {
            // Find last char boundary before the truncation point.
            // Iterate on the full string to avoid slicing at a non-boundary.
            let trunc = max_goal.saturating_sub(1);
            let end = task
                .goal
                .char_indices()
                .take_while(|(i, _)| *i < trunc)
                .last()
                .map_or(0, |(i, c)| i + c.len_utf8());
            format!("{}…", &task.goal[..end])
        } else {
            task.goal.clone()
        };

        let style = phase_style(task.phase);
        let line = Line::from(vec![
            Span::styled(format!("{indent}{indicator} "), style),
            Span::styled(goal_display, style),
            Span::styled(cursor.to_string(), Style::default().fg(Color::Cyan)),
        ]);
        lines.push(line);

        // Push children in reverse for correct DFS order.
        for child_id in task.subtask_ids.iter().rev() {
            stack.push(*child_id);
        }
    }

    if lines.is_empty() {
        lines.push(Line::from("  (no tasks)"));
    }

    lines
}

pub struct TaskTreeWidget<'a> {
    pub tasks: &'a HashMap<TaskId, TuiTask>,
    pub root_id: Option<TaskId>,
    pub current_task: Option<TaskId>,
    pub scroll_offset: usize,
}

impl Widget for TaskTreeWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(" Task Tree ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        block.render(area, buf);

        let lines = render_tree_lines(self.tasks, self.root_id, self.current_task, inner.width);
        let max_scroll = lines.len().saturating_sub(inner.height as usize);
        let clamped_offset = self.scroll_offset.min(max_scroll);
        let visible_lines: Vec<Line<'_>> = lines
            .into_iter()
            .skip(clamped_offset)
            .take(inner.height as usize)
            .collect();

        let text = Text::from(visible_lines);
        text.render(inner, buf);
    }
}
