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

    let goal = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "No goal provided — pass a goal as the first argument.".into());

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

    let (tx, _rx) = event_channel();
    let mut orchestrator = Orchestrator::new(agent, state, tx);

    let outcome = orchestrator.run(root_id).await?;
    println!("Epic completed: {outcome:?}");

    Ok(())
}
