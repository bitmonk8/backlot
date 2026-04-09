//! Runtime execution primitives.
//!
//! Deliverable 9 lands the prompt block executor and the `AgentExecutor` seam
//! used to inject a fake agent in tests. Subsequent deliverables will add the
//! call block executor, transition scheduling, and the function-level and
//! workflow-level drivers.

pub mod agent;
pub mod prompt;

pub use agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
pub use prompt::{ResolvedAgentConfig, execute_prompt_block, resolve_agent_config};
