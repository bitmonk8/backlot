mod agent;
mod cli;
mod config;
mod events;
mod init;
mod orchestrator;
mod sandbox;
mod state;
mod task;
mod tui;

#[cfg(test)]
pub(crate) mod test_support;

use agent::flick::FlickAgent;
use cli::{Cli, Command};
use config::project::EpicConfig;
use events::event_channel;
use orchestrator::Orchestrator;
use state::EpicState;
use task::Task;
use tui::TuiApp;

use anyhow::bail;
use clap::Parser;
use std::time::Duration;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let project_root = std::env::current_dir()?;
    let work_dir = project_root.join(".epic");
    let state_path = work_dir.join("state.json");

    if matches!(&cli.command, Command::Status) {
        return print_status(&state_path);
    }

    if matches!(&cli.command, Command::Run { .. } | Command::Resume)
        && !cli.no_sandbox_warn
        && !sandbox::detect_virtualization()
    {
        eprintln!(
            "Warning: No container or VM detected. Running epic outside an isolated environment \
             is not recommended — agents execute arbitrary shell commands. See epic documentation \
             for container setup guidance."
        );
    }

    // Check epic.toml existence before constructing agent (avoids requiring credentials for this error).
    if matches!(&cli.command, Command::Init) {
        let config_path = project_root.join("epic.toml");
        if config_path.exists() {
            bail!(
                "epic.toml already exists at {}. Delete it first to reinitialize.",
                config_path.display()
            );
        }
    }

    std::fs::create_dir_all(&work_dir)?;

    let config_path = project_root.join("epic.toml");
    let epic_config = EpicConfig::load(&config_path)?;

    let timeout = Duration::from_secs(300);

    let agent = FlickAgent::new(
        project_root.clone(),
        cli.credential,
        timeout,
        epic_config.models.clone(),
        epic_config.verification_steps.clone(),
    );

    if matches!(&cli.command, Command::Init) {
        return init::run_init(&agent, &project_root).await;
    }

    let (state, root_id, goal_text) = match &cli.command {
        Command::Run { goal } => {
            if state_path.exists() {
                let (existing, rid, persisted_goal) = load_and_validate_state(&state_path)?;
                if *goal != persisted_goal {
                    bail!(
                        "State file exists with different goal: \"{persisted_goal}\". \
                         Use `epic resume` to continue, or delete .epic/state.json to start fresh."
                    );
                }
                eprintln!("Resuming from {}", state_path.display());
                (existing, rid, persisted_goal)
            } else {
                let mut state = EpicState::new();
                let root_id = state.next_task_id();
                let root = Task::new(
                    root_id,
                    None,
                    goal.clone(),
                    vec!["Task completed successfully".into()],
                    0,
                );
                state.insert(root);
                state.set_root_id(root_id);
                (state, root_id, goal.clone())
            }
        }
        Command::Resume => {
            if !state_path.exists() {
                bail!(
                    "No state file found at {}. Nothing to resume.",
                    state_path.display()
                );
            }
            let (state, root_id, goal_text) = load_and_validate_state(&state_path)?;
            eprintln!("Resuming from {}", state_path.display());
            (state, root_id, goal_text)
        }
        Command::Init | Command::Status => unreachable!(),
    };

    let (tx, rx) = event_channel();
    let mut orchestrator = Orchestrator::new(agent, state, tx)
        .with_limits(epic_config.limits)
        .with_state_path(state_path.clone())
        .with_project_root(project_root.clone());

    if cli.no_tui {
        drop(rx);
        let outcome = orchestrator.run(root_id).await?;
        let state = orchestrator.into_state();
        state.save(&state_path)?;
        println!("Epic completed: {outcome:?}");
    } else {
        let mut tui_app = TuiApp::new(goal_text);

        let orch_handle = tokio::spawn(async move {
            let result = orchestrator.run(root_id).await;
            let state = orchestrator.into_state();
            (result, state)
        });

        let tui_result = tokio::task::spawn_blocking(move || tui_app.run(rx)).await?;

        let abort_handle = orch_handle.abort_handle();
        let mut saved = false;
        match tokio::time::timeout(Duration::from_secs(2), orch_handle).await {
            Ok(Ok((result, state))) => {
                state.save(&state_path)?;
                saved = true;
                if let Ok(outcome) = result {
                    println!("Epic completed: {outcome:?}");
                } else if let Err(e) = result {
                    eprintln!("Orchestrator error: {e}");
                }
            }
            Ok(Err(e)) => {
                eprintln!("Orchestrator task panicked: {e}");
            }
            Err(_) => {
                abort_handle.abort();
                eprintln!("Orchestrator still running after timeout, aborted.");
            }
        }
        if !saved {
            eprintln!("State was preserved up to the last checkpoint.");
            eprintln!("Resume with: epic resume");
        }

        tui_result?;
    }

    Ok(())
}

fn load_and_validate_state(
    state_path: &std::path::Path,
) -> anyhow::Result<(EpicState, task::TaskId, String)> {
    let state = EpicState::load(state_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to load state from {}: {e}. Delete the file to start fresh or fix the JSON.",
            state_path.display()
        )
    })?;
    let Some(root_id) = state.root_id() else {
        bail!(
            "State file at {} is corrupt (no root task). Delete it to start fresh.",
            state_path.display()
        );
    };
    let Some(root_task) = state.get(root_id) else {
        bail!(
            "State file at {} is corrupt (root task missing). Delete it to start fresh.",
            state_path.display()
        );
    };
    let goal = root_task.goal.clone();
    Ok((state, root_id, goal))
}

fn print_status(state_path: &std::path::Path) -> anyhow::Result<()> {
    if !state_path.exists() {
        println!("No active run (no state file at {}).", state_path.display());
        return Ok(());
    }
    let (state, root_id, _) = load_and_validate_state(state_path)?;
    let root = state.get(root_id).expect("validated by load_and_validate_state");

    println!("Goal: {}", root.goal);
    println!("Root status: {:?}", root.phase);
    println!();

    let ids = state.dfs_order(root_id);
    let mut completed = 0u32;
    let mut failed = 0u32;
    let mut in_progress = 0u32;
    let mut pending = 0u32;
    for &id in &ids {
        if let Some(t) = state.get(id) {
            match t.phase {
                task::TaskPhase::Completed => completed += 1,
                task::TaskPhase::Failed => failed += 1,
                task::TaskPhase::Pending => pending += 1,
                _ => in_progress += 1,
            }
        }
    }

    let total = ids.len();
    println!(
        "Tasks: {total} total, {completed} completed, {in_progress} in-progress, {pending} pending, {failed} failed"
    );

    Ok(())
}
