// TUI application: read-only monitoring via ratatui + crossterm.

pub mod metrics;
pub mod task_tree;
pub mod worklog;

use crate::events::{Event, EventReceiver};
use crate::task::{TaskId, TaskOutcome, TaskPhase};
use crate::tui::metrics::MetricsWidget;
use crate::tui::task_tree::{TaskTreeWidget, TuiTask};
use crate::tui::worklog::{WorklogEntry, WorklogWidget};
use anyhow::Result;
use crossterm::event::{self as ct_event, EventStream, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

const MAX_WORKLOG_ENTRIES: usize = 10_000;

pub struct TuiApp {
    tasks: HashMap<TaskId, TuiTask>,
    root_id: Option<TaskId>,
    current_task: Option<TaskId>,
    worklog: Vec<WorklogEntry>,
    follow_tail: bool,
    show_metrics: bool,
    tree_scroll: usize,
    root_goal: String,
    session_start: Instant,
    orchestrator_done: bool,
    total_cost_usd: f64,
}

impl TuiApp {
    pub fn new(root_goal: String) -> Self {
        Self {
            tasks: HashMap::new(),
            root_id: None,
            current_task: None,
            worklog: Vec::new(),
            follow_tail: true,
            show_metrics: false,
            tree_scroll: 0,
            root_goal,
            session_start: Instant::now(),
            orchestrator_done: false,
            total_cost_usd: 0.0,
        }
    }

    pub async fn run(&mut self, mut event_rx: EventReceiver) -> Result<()> {
        enable_raw_mode()?;
        crossterm::execute!(io::stdout(), EnterAlternateScreen)?;

        // Restore terminal on panic so the user's shell isn't left broken.
        // We wrap the original hook so it still runs after cleanup.
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
            original_hook(info);
        }));

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        let result = self.event_loop(&mut terminal, &mut event_rx).await;

        // Restore original panic hook. Our hook captured and consumed it,
        // so take_hook returns it (with the terminal-cleanup wrapper). On
        // normal exit the default hook is sufficient since the terminal is
        // about to be restored below.
        let _ = std::panic::take_hook();
        disable_raw_mode()?;
        crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;

        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        event_rx: &mut EventReceiver,
    ) -> Result<()> {
        let mut ct_stream = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(50));

        loop {
            terminal.draw(|frame| self.render(frame))?;

            tokio::select! {
                _ = tick.tick() => {}
                ct_event = ct_stream.next() => {
                    if let Some(Ok(ct_event::Event::Key(key))) = ct_event {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                                return Ok(());
                            }
                            (KeyCode::Char('t'), _) => {
                                self.follow_tail = !self.follow_tail;
                            }
                            (KeyCode::Char('m'), _) => {
                                self.show_metrics = !self.show_metrics;
                            }
                            (KeyCode::Up, _) => {
                                self.tree_scroll = self.tree_scroll.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                self.tree_scroll = (self.tree_scroll + 1).min(self.tasks.len());
                            }
                            _ => {}
                        }
                    }
                }
                event = event_rx.recv() => {
                    match event {
                        Some(event) => self.handle_event(event),
                        None => {
                            self.orchestrator_done = true;
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)] // One match arm per event variant; splitting adds indirection.
    fn handle_event(&mut self, event: Event) {
        match event {
            Event::TaskRegistered {
                task_id,
                parent_id,
                goal,
                depth,
            } => {
                if self.root_id.is_none() {
                    self.root_id = Some(task_id);
                }
                if let Some(parent) = parent_id.and_then(|pid| self.tasks.get_mut(&pid)) {
                    if !parent.subtask_ids.contains(&task_id) {
                        parent.subtask_ids.push(task_id);
                    }
                }
                self.tasks.entry(task_id).or_insert(TuiTask {
                    id: task_id,
                    parent_id,
                    goal,
                    depth,
                    phase: TaskPhase::Pending,
                    path: None,
                    subtask_ids: Vec::new(),
                    current: false,
                    cost_usd: 0.0,
                });
            }
            Event::PhaseTransition { task_id, phase } => {
                let is_active = matches!(
                    phase,
                    TaskPhase::Assessing | TaskPhase::Executing | TaskPhase::Verifying
                );
                if let Some(task) = self.tasks.get_mut(&task_id) {
                    let goal = task.goal.clone();
                    task.phase = phase;
                    task.current = is_active;
                    if is_active {
                        self.current_task = Some(task_id);
                    }
                    self.worklog.push(WorklogEntry::info(
                        format!("{task_id} → {phase:?}: {goal}"),
                        self.session_start,
                    ));
                }
            }
            Event::PathSelected { task_id, path } => {
                if let Some(task) = self.tasks.get_mut(&task_id) {
                    task.path = Some(path.clone());
                }
                self.worklog.push(WorklogEntry::info(
                    format!("{task_id} path: {path:?}"),
                    self.session_start,
                ));
            }
            Event::ModelSelected { task_id, model } => {
                self.worklog.push(WorklogEntry::info(
                    format!("{task_id} model: {model:?}"),
                    self.session_start,
                ));
            }
            Event::ModelEscalated { task_id, from, to } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} escalated: {from:?} → {to:?}"),
                    self.session_start,
                ));
            }
            Event::SubtasksCreated {
                parent_id,
                child_ids,
            } => {
                self.worklog.push(WorklogEntry::info(
                    format!("{parent_id} decomposed into {} subtasks", child_ids.len()),
                    self.session_start,
                ));
            }
            Event::TaskCompleted { task_id, outcome } => {
                if let Some(task) = self.tasks.get_mut(&task_id) {
                    task.current = false;
                    // Defensive: set phase even if PhaseTransition arrived first.
                    task.phase = match &outcome {
                        TaskOutcome::Success => TaskPhase::Completed,
                        TaskOutcome::Failed { .. } => TaskPhase::Failed,
                    };
                }
                if self.current_task == Some(task_id) {
                    self.current_task = None;
                }
                match &outcome {
                    TaskOutcome::Success => {
                        self.worklog.push(WorklogEntry::success(
                            format!("{task_id} completed"),
                            self.session_start,
                        ));
                    }
                    TaskOutcome::Failed { reason } => {
                        self.worklog.push(WorklogEntry::error(
                            format!("{task_id} failed: {reason}"),
                            self.session_start,
                        ));
                    }
                }
            }
            Event::RetryAttempt {
                task_id,
                attempt,
                model,
            } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} retry #{attempt} ({model:?})"),
                    self.session_start,
                ));
            }
            Event::DiscoveriesRecorded { task_id, count } => {
                self.worklog.push(WorklogEntry::info(
                    format!("{task_id} recorded {count} discovery(ies)"),
                    self.session_start,
                ));
            }
            Event::CheckpointAdjust { task_id } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} checkpoint: adjusting pending subtasks"),
                    self.session_start,
                ));
            }
            Event::CheckpointEscalate { task_id } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} checkpoint: escalating to recovery"),
                    self.session_start,
                ));
            }
            Event::FixAttempt {
                task_id,
                attempt,
                model,
            } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} fix attempt #{attempt} ({model:?})"),
                    self.session_start,
                ));
            }
            Event::FixModelEscalated { task_id, from, to } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} fix escalated: {from:?} → {to:?}"),
                    self.session_start,
                ));
            }
            Event::BranchFixRound {
                task_id,
                round,
                model,
            } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} branch fix round {round} ({model:?})"),
                    self.session_start,
                ));
            }
            Event::FixSubtasksCreated {
                task_id,
                count,
                round,
            } => {
                self.worklog.push(WorklogEntry::info(
                    format!("{task_id} created {count} fix subtask(s) (round {round})"),
                    self.session_start,
                ));
            }
            Event::RecoveryStarted { task_id, round } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} recovery round {round} started"),
                    self.session_start,
                ));
            }
            Event::RecoveryPlanSelected {
                task_id,
                ref approach,
            } => {
                self.worklog.push(WorklogEntry::warn(
                    format!("{task_id} recovery approach: {approach}"),
                    self.session_start,
                ));
            }
            Event::RecoverySubtasksCreated {
                task_id,
                count,
                round,
            } => {
                self.worklog.push(WorklogEntry::info(
                    format!("{task_id} created {count} recovery subtask(s) (round {round})"),
                    self.session_start,
                ));
            }
            Event::TaskLimitReached { task_id } => {
                self.worklog.push(WorklogEntry::error(
                    format!("{task_id} task limit reached — no new subtasks created"),
                    self.session_start,
                ));
            }
            Event::UsageUpdated {
                task_id,
                phase_cost_usd,
                total_cost_usd,
            } => {
                if let Some(task) = self.tasks.get_mut(&task_id) {
                    task.cost_usd = total_cost_usd;
                }
                self.total_cost_usd += phase_cost_usd;
            }
            Event::FileLevelReviewCompleted { task_id, passed } => {
                if passed {
                    self.worklog.push(WorklogEntry::success(
                        format!("{task_id} file-level review passed"),
                        self.session_start,
                    ));
                } else {
                    self.worklog.push(WorklogEntry::warn(
                        format!("{task_id} file-level review failed"),
                        self.session_start,
                    ));
                }
            }
            Event::VaultBootstrapCompleted { cost_usd } => {
                self.worklog.push(WorklogEntry::info(
                    format!("vault bootstrap completed (${cost_usd:.4})"),
                    self.session_start,
                ));
            }
            Event::VaultRecorded {
                task_id, document, ..
            } => {
                self.worklog.push(WorklogEntry::info(
                    format!("{task_id} recorded to vault: {document}"),
                    self.session_start,
                ));
            }
            Event::VaultReorganizeCompleted {
                merged,
                restructured,
                deleted,
            } => {
                self.worklog.push(WorklogEntry::info(
                    format!(
                        "vault reorganized: {merged} merged, {restructured} restructured, {deleted} deleted"
                    ),
                    self.session_start,
                ));
            }
        }

        // Evict oldest entries if worklog exceeds cap.
        if self.worklog.len() > MAX_WORKLOG_ENTRIES {
            let drain = self.worklog.len() - MAX_WORKLOG_ENTRIES;
            self.worklog.drain(..drain);
        }
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();

        // Layout: header (2), body (fill), footer (1).
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area);

        self.render_header(frame, outer[0]);
        self.render_body(frame, outer[1]);
        self.render_footer(frame, outer[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let elapsed = self.session_start.elapsed();
        let total = self.tasks.len();
        let completed = self
            .tasks
            .values()
            .filter(|t| t.phase == TaskPhase::Completed)
            .count();

        let status = if self.orchestrator_done {
            "DONE"
        } else {
            "RUNNING"
        };

        let cost = self.total_cost_usd;
        let header_text = format!(
            " [{status}] {goal}  ({completed}/{total} tasks, ${cost:.4}, {elapsed:.0?})",
            goal = self.root_goal,
        );

        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        let paragraph = Paragraph::new(header_text)
            .style(Style::default().fg(Color::White).bold())
            .block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_body(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.show_metrics {
            // Three columns: tree | worklog | metrics.
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(50),
                    Constraint::Percentage(20),
                ])
                .split(area);

            self.render_task_tree(frame, columns[0]);
            self.render_worklog(frame, columns[1]);
            frame.render_widget(
                MetricsWidget {
                    tasks: &self.tasks,
                    total_cost_usd: self.total_cost_usd,
                },
                columns[2],
            );
        } else {
            // Two columns: tree | worklog.
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(area);

            self.render_task_tree(frame, columns[0]);
            self.render_worklog(frame, columns[1]);
        }
    }

    fn render_task_tree(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(
            TaskTreeWidget {
                tasks: &self.tasks,
                root_id: self.root_id,
                current_task: self.current_task,
                scroll_offset: self.tree_scroll,
            },
            area,
        );
    }

    fn render_worklog(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(
            WorklogWidget {
                entries: &self.worklog,
                follow_tail: self.follow_tail,
            },
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let tail_indicator = if self.follow_tail { "on" } else { "off" };
        let metrics_indicator = if self.show_metrics { "on" } else { "off" };
        let footer = format!(
            " q: quit  t: tail [{tail_indicator}]  m: metrics [{metrics_indicator}]  ↑↓: scroll tree"
        );
        let paragraph = Paragraph::new(footer).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(paragraph, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskOutcome, TaskPath};

    fn app() -> TuiApp {
        TuiApp::new("test goal".into())
    }

    fn register(app: &mut TuiApp, id: u64, parent: Option<u64>, goal: &str, depth: u32) {
        app.handle_event(Event::TaskRegistered {
            task_id: TaskId(id),
            parent_id: parent.map(TaskId),
            goal: goal.into(),
            depth,
        });
    }

    #[test]
    fn task_registration_sets_root() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        assert_eq!(app.root_id, Some(TaskId(0)));
        assert_eq!(app.tasks.len(), 1);
    }

    #[test]
    fn duplicate_registration_ignored() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        register(&mut app, 0, None, "root again", 0);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[&TaskId(0)].goal, "root");
    }

    #[test]
    fn child_registration_links_parent() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        register(&mut app, 1, Some(0), "child", 1);
        assert_eq!(app.tasks[&TaskId(0)].subtask_ids, vec![TaskId(1)]);
    }

    #[test]
    fn phase_transition_tracks_current() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        app.handle_event(Event::PhaseTransition {
            task_id: TaskId(0),
            phase: TaskPhase::Executing,
        });
        assert_eq!(app.current_task, Some(TaskId(0)));
        assert!(app.tasks[&TaskId(0)].current);
    }

    #[test]
    fn task_completion_clears_current() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        app.handle_event(Event::PhaseTransition {
            task_id: TaskId(0),
            phase: TaskPhase::Executing,
        });
        app.handle_event(Event::TaskCompleted {
            task_id: TaskId(0),
            outcome: TaskOutcome::Success,
        });
        assert_eq!(app.current_task, None);
        assert_eq!(app.tasks[&TaskId(0)].phase, TaskPhase::Completed);
    }

    #[test]
    fn task_failure_sets_failed_phase() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        app.handle_event(Event::TaskCompleted {
            task_id: TaskId(0),
            outcome: TaskOutcome::Failed {
                reason: "oops".into(),
            },
        });
        assert_eq!(app.tasks[&TaskId(0)].phase, TaskPhase::Failed);
    }

    #[test]
    fn path_selection_stored() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        app.handle_event(Event::PathSelected {
            task_id: TaskId(0),
            path: TaskPath::Leaf,
        });
        assert_eq!(app.tasks[&TaskId(0)].path, Some(TaskPath::Leaf));
    }

    #[test]
    fn worklog_eviction_at_cap() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        for _ in 0..MAX_WORKLOG_ENTRIES + 50 {
            app.handle_event(Event::ModelSelected {
                task_id: TaskId(0),
                model: crate::task::Model::Haiku,
            });
        }
        assert!(app.worklog.len() <= MAX_WORKLOG_ENTRIES);
    }

    #[test]
    fn scroll_clamped_to_task_count() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        app.tree_scroll = 100;
        let clamped = (app.tree_scroll + 1).min(app.tasks.len());
        assert_eq!(clamped, 1);
    }

    #[test]
    fn usage_updated_accumulates_cost() {
        let mut app = app();
        register(&mut app, 0, None, "root", 0);
        app.handle_event(Event::UsageUpdated {
            task_id: TaskId(0),
            phase_cost_usd: 0.01,
            total_cost_usd: 0.01,
        });
        assert!((app.total_cost_usd - 0.01).abs() < f64::EPSILON);
        assert!((app.tasks[&TaskId(0)].cost_usd - 0.01).abs() < f64::EPSILON);

        app.handle_event(Event::UsageUpdated {
            task_id: TaskId(0),
            phase_cost_usd: 0.02,
            total_cost_usd: 0.03,
        });
        assert!((app.total_cost_usd - 0.03).abs() < f64::EPSILON);
        assert!((app.tasks[&TaskId(0)].cost_usd - 0.03).abs() < f64::EPSILON);
    }
}
