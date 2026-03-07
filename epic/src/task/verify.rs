// Verification phase: build/lint/test gates, review, fix loops.
//
// Design note: EPIC_DESIGN2 specifies file-level review and simplification review
// as additional verification sub-phases. These are deferred to post-v1; the current
// verification model uses a single verification agent with configurable commands.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationOutcome {
    Pass,
    Fail { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub outcome: VerificationOutcome,
    pub details: String,
}
