// Shared infrastructure passed to tasks during execution.

use crate::agent::AgentService;
use crate::config::project::LimitsConfig;
use crate::events::EventSender;
use std::path::PathBuf;
use std::sync::Arc;

#[allow(dead_code)] // Used by legacy orchestrator retained for test migration
pub struct Services<A: AgentService> {
    pub agent: A,
    pub events: EventSender,
    pub vault: Option<Arc<vault::Vault>>,
    pub limits: LimitsConfig,
    pub project_root: Option<PathBuf>,
    pub state_path: Option<PathBuf>,
}
