// Verification phase: build/lint/test gates, review, fix loops.
//
// Design note: File-level review is implemented as a post-verification sub-phase
// for leaf tasks (catches intent mismatches that gates cannot detect).
// Simplification review remains deferred.

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

/// Simplified pass/fail used by retry and fix loops.
pub enum VerifyOutcome {
    Passed,
    Failed(String),
}
