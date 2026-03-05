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

use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Minimal wiring — just enough to construct and run the orchestrator.
    // A proper CLI (clap) is a follow-up.
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

    // Resume from persisted state, or create fresh state from CLI goal.
    let (state, root_id) = if state_path.exists() {
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

        // Detect goal mismatch if a CLI goal was provided.
        if let Some(ref goal) = cli_goal {
            let persisted_goal = state.get(root_id).map_or("", |t| t.goal.as_str());
            if goal != persisted_goal {
                eprintln!(
                    "State file exists with different goal: \"{persisted_goal}\". \
                     Use --resume to continue, or delete .epic/state.json to start fresh."
                );
                std::process::exit(1);
            }
        }

        eprintln!("Resuming from {}", state_path.display());
        (state, root_id)
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
            goal,
            vec!["Task completed successfully".into()],
            0,
        );
        state.insert(root);
        state.set_root_id(root_id);
        (state, root_id)
    };

    let (tx, _rx) = event_channel();
    let mut orchestrator = Orchestrator::new(agent, state, tx)
        .with_state_path(state_path.clone());

    let outcome = orchestrator.run(root_id).await?;

    // Final save.
    let state = orchestrator.into_state();
    state.save(&state_path)?;
    println!("Epic completed: {outcome:?}");

    Ok(())
}
