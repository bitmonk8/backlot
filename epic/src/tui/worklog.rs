// Worklog panel: event-level updates from orchestrator.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Widget};
use std::time::Instant;

pub struct WorklogEntry {
    #[allow(dead_code)] // Kept for structured log export.
    pub timestamp: Instant,
    pub text: String,
    pub style: Style,
}

#[allow(clippy::needless_pass_by_value)] // Callers always pass freshly-formatted Strings.
impl WorklogEntry {
    pub fn info(text: String, session_start: Instant) -> Self {
        Self {
            timestamp: Instant::now(),
            text: format!("[{:>6.1}s] {text}", session_start.elapsed().as_secs_f64()),
            style: Style::default(),
        }
    }

    pub fn success(text: String, session_start: Instant) -> Self {
        Self {
            timestamp: Instant::now(),
            text: format!("[{:>6.1}s] {text}", session_start.elapsed().as_secs_f64()),
            style: Style::default().fg(Color::Green),
        }
    }

    pub fn error(text: String, session_start: Instant) -> Self {
        Self {
            timestamp: Instant::now(),
            text: format!("[{:>6.1}s] {text}", session_start.elapsed().as_secs_f64()),
            style: Style::default().fg(Color::Red),
        }
    }

    pub fn warn(text: String, session_start: Instant) -> Self {
        Self {
            timestamp: Instant::now(),
            text: format!("[{:>6.1}s] {text}", session_start.elapsed().as_secs_f64()),
            style: Style::default().fg(Color::Yellow),
        }
    }
}

pub struct WorklogWidget<'a> {
    pub entries: &'a [WorklogEntry],
    pub follow_tail: bool,
}

impl Widget for WorklogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(" Worklog ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        block.render(area, buf);

        let visible_height = inner.height as usize;
        let start = if self.follow_tail {
            self.entries.len().saturating_sub(visible_height)
        } else {
            0
        };

        let lines: Vec<Line<'_>> = self
            .entries
            .iter()
            .skip(start)
            .take(visible_height)
            .map(|e| Line::from(Span::styled(e.text.clone(), e.style)))
            .collect();

        let text = Text::from(lines);
        text.render(inner, buf);
    }
}
