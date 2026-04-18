//! Shared test fixtures and assertions for the `exec` submodules.
//!
//! Lifted from byte-identical copies that previously appeared in
//! `function.rs`, `schedule.rs`, and `dataflow.rs`. Gated `#[cfg(test)]`
//! at the module declaration site (`exec/mod.rs`), so this file is only
//! compiled into test builds.

use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;

use crate::error::MechError;
use crate::exec::BoxFuture;
use crate::exec::agent::{AgentExecutor, AgentRequest, AgentResponse};

/// Capturing fake agent: records every inbound [`AgentRequest`] and
/// returns the next pre-queued JSON output. Replaces ~12 byte-identical
/// `struct CapturingAgent { requests, responses }` definitions across the
/// `exec` test modules.
pub(crate) struct CapturingAgent {
    pub requests: Arc<Mutex<Vec<AgentRequest>>>,
    pub responses: Mutex<Vec<JsonValue>>,
}

impl CapturingAgent {
    /// Build a capturing agent with the given queued responses. Callers
    /// that need to inspect the captured request log should clone the
    /// `requests` field after construction:
    /// `let captured = agent.requests.clone();`
    pub fn new(responses: Vec<JsonValue>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Mutex::new(responses),
        }
    }
}

impl AgentExecutor for CapturingAgent {
    fn run<'a>(&'a self, request: AgentRequest) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
        self.requests.lock().unwrap().push(request);
        let mut q = self.responses.lock().unwrap();
        assert!(
            !q.is_empty(),
            "CapturingAgent: response queue exhausted \u{2014} agent received more requests than queued responses"
        );
        let output = q.remove(0);
        Box::pin(async move {
            Ok(AgentResponse {
                output,
                messages: vec![],
            })
        })
    }
}

/// Assert every captured request's `system` field matches the expected
/// rendered string. Used by the system-forwarding tests in
/// `function.rs`, `schedule.rs`, and `dataflow.rs`.
pub(crate) fn assert_all_requests_have_system(reqs: &[AgentRequest], expected: &str) {
    assert!(
        !reqs.is_empty(),
        "assert_all_requests_have_system: no requests captured (vacuous pass guard)"
    );
    for (i, r) in reqs.iter().enumerate() {
        assert_eq!(
            r.system.as_deref(),
            Some(expected),
            "request {i}: system must equal {expected:?}, got {:?}",
            r.system
        );
    }
}

/// Assert every captured request has an empty conversation history —
/// the §4.6 rule 3 invariant for dataflow blocks.
pub(crate) fn assert_dataflow_history_empty(reqs: &[AgentRequest]) {
    assert!(
        !reqs.is_empty(),
        "assert_dataflow_history_empty: no requests captured (vacuous pass guard)"
    );
    for (i, r) in reqs.iter().enumerate() {
        assert!(
            r.history.is_empty(),
            "request {i}: dataflow blocks must run with empty history; got {:?}",
            r.history
        );
    }
}
