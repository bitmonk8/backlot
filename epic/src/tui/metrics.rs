// Metrics display: task counts by phase.

use crate::task::TaskPhase;
use crate::tui::task_tree::TuiTask;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Widget};
use std::collections::HashMap;

use crate::task::TaskId;

pub struct MetricsWidget<'a> {
    pub tasks: &'a HashMap<TaskId, TuiTask>,
}

impl Widget for MetricsWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(" Metrics ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        block.render(area, buf);

        let total = self.tasks.len();
        let completed = self
            .tasks
            .values()
            .filter(|t| t.phase == TaskPhase::Completed)
            .count();
        let failed = self
            .tasks
            .values()
            .filter(|t| t.phase == TaskPhase::Failed)
            .count();
        let in_progress = self
            .tasks
            .values()
            .filter(|t| {
                matches!(
                    t.phase,
                    TaskPhase::Assessing | TaskPhase::Executing | TaskPhase::Verifying
                )
            })
            .count();
        let pending = self
            .tasks
            .values()
            .filter(|t| t.phase == TaskPhase::Pending)
            .count();

        let lines = vec![
            Line::from(vec![
                Span::styled("Total:       ", Style::default()),
                Span::styled(total.to_string(), Style::default().fg(Color::White).bold()),
            ]),
            Line::from(vec![
                Span::styled("Completed:   ", Style::default()),
                Span::styled(
                    completed.to_string(),
                    Style::default().fg(Color::Green).bold(),
                ),
            ]),
            Line::from(vec![
                Span::styled("In progress: ", Style::default()),
                Span::styled(
                    in_progress.to_string(),
                    Style::default().fg(Color::Yellow).bold(),
                ),
            ]),
            Line::from(vec![
                Span::styled("Pending:     ", Style::default()),
                Span::styled(
                    pending.to_string(),
                    Style::default().fg(Color::DarkGray).bold(),
                ),
            ]),
            Line::from(vec![
                Span::styled("Failed:      ", Style::default()),
                Span::styled(
                    failed.to_string(),
                    Style::default().fg(Color::Red).bold(),
                ),
            ]),
        ];

        let text = Text::from(lines);
        text.render(inner, buf);
    }
}
