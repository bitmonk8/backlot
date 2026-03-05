mod agent;
mod config;
mod events;
mod git;
mod metrics;
mod orchestrator;
mod services;
mod state;
mod task;
mod tui;

use agent::flick::FlickAgent;
use events::event_channel;
use orchestrator::Orchestrator;
use state::EpicState;
use task::Task;
use tui::TuiApp;

use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> anyhow::Result<()> {
    let flick_path = std::env::var("EPIC_FLICK_PATH")
        .map_or_else(|_| PathBuf::from("flick"), PathBuf::from);

    let project_root = std::env::current_dir()?;
    let work_dir = project_root.join(".epic");
    std::fs::create_dir_all(&work_dir)?;
    let state_path = work_dir.join("state.json");
    let credential = std::env::var("EPIC_CREDENTIAL")
        .unwrap_or_else(|_| "anthropic".into());
    let timeout = Duration::from_secs(300);

    let agent = FlickAgent::new(
        flick_path,
        project_root,
        work_dir,
        credential,
        timeout,
    )
    .await?;

    let cli_goal = std::env::args().nth(1);
    let no_tui = std::env::var("EPIC_NO_TUI").is_ok();

    // Resume from persisted state, or create fresh state from CLI goal.
    let (state, root_id, goal_text) = if state_path.exists() {
        let state = match EpicState::load(&state_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "Failed to load state from {}: {e}. Delete the file to start fresh or fix the JSON.",
                    state_path.display()
                );
                std::process::exit(1);
            }
        };
        let root_id = state
            .root_id()
            .ok_or_else(|| anyhow::anyhow!("persisted state has no root_id"))?;

        let persisted_goal = state
            .get(root_id)
            .map_or_else(String::new, |t| t.goal.clone());

        if let Some(ref goal) = cli_goal {
            if goal != &persisted_goal {
                eprintln!(
                    "State file exists with different goal: \"{persisted_goal}\". \
                     Use --resume to continue, or delete .epic/state.json to start fresh."
                );
                std::process::exit(1);
            }
        }

        eprintln!("Resuming from {}", state_path.display());
        (state, root_id, persisted_goal)
    } else {
        let Some(goal) = cli_goal else {
            eprintln!("Usage: epic <goal>");
            std::process::exit(1);
        };

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
        (state, root_id, goal)
    };

    let (tx, rx) = event_channel();
    let mut orchestrator = Orchestrator::new(agent, state, tx)
        .with_state_path(state_path.clone());

    if no_tui {
        // Headless mode: run orchestrator directly, discard events.
        let outcome = orchestrator.run(root_id).await?;
        let state = orchestrator.into_state();
        state.save(&state_path)?;
        println!("Epic completed: {outcome:?}");
    } else {
        // TUI mode: orchestrator in background, TUI in blocking thread.
        let mut tui_app = TuiApp::new(goal_text);

        let orch_handle = tokio::spawn(async move {
            let result = orchestrator.run(root_id).await;
            let state = orchestrator.into_state();
            (result, state)
        });

        // TUI is a blocking crossterm loop — run on a blocking thread.
        let tui_result =
            tokio::task::spawn_blocking(move || tui_app.run(rx)).await?;

        // Give orchestrator 2s to finish gracefully, then abort.
        let abort_handle = orch_handle.abort_handle();
        match tokio::time::timeout(Duration::from_secs(2), orch_handle).await {
            Ok(Ok((result, state))) => {
                state.save(&state_path)?;
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

        tui_result?;
    }

    Ok(())
}
