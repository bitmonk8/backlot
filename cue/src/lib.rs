// Cue: generic recursive task orchestration framework.
// Defines coordination algorithm and trait contracts. Application crates
// provide concrete TaskNode and TaskStore implementations.

pub mod config;
pub mod context;
pub mod events;
pub mod orchestrator;
pub mod traits;
pub mod types;

// Re-export primary public API.
pub use config::LimitsConfig;
pub use context::TreeContext;
pub use events::CueEvent;
pub use orchestrator::{Orchestrator, OrchestratorError};
pub use traits::{TaskNode, TaskStore};
pub use types::*;
