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

#[tokio::main]
async fn main() {
    println!("epic — orchestrator compiled, no agent wired yet");
}
