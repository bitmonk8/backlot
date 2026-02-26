// Assessment phase: determine leaf vs branch path, select model tier.

use super::{Model, TaskPath};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssessmentResult {
    pub path: TaskPath,
    pub model: Model,
    pub rationale: String,
}
