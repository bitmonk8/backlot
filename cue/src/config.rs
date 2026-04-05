// Orchestration limits and verification step configuration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_max_recovery_rounds")]
    pub max_recovery_rounds: u32,
    #[serde(default = "default_retry_budget")]
    pub retry_budget: u32,
    #[serde(default = "default_branch_fix_rounds")]
    pub branch_fix_rounds: u32,
    #[serde(default = "default_root_fix_rounds")]
    pub root_fix_rounds: u32,
    #[serde(default = "default_max_total_tasks")]
    pub max_total_tasks: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationStep {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
}

const fn default_max_depth() -> u32 {
    8
}
const fn default_max_recovery_rounds() -> u32 {
    2
}
const fn default_retry_budget() -> u32 {
    3
}
const fn default_branch_fix_rounds() -> u32 {
    3
}
const fn default_root_fix_rounds() -> u32 {
    4
}
const fn default_max_total_tasks() -> u32 {
    100
}
const fn default_timeout() -> u32 {
    300
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            max_recovery_rounds: default_max_recovery_rounds(),
            retry_budget: default_retry_budget(),
            branch_fix_rounds: default_branch_fix_rounds(),
            root_fix_rounds: default_root_fix_rounds(),
            max_total_tasks: default_max_total_tasks(),
        }
    }
}
