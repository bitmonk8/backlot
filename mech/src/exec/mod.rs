//! Runtime execution primitives.
//!
//! Deliverable 9 lands the prompt block executor and the `AgentExecutor` seam
//! used to inject a fake agent in tests. Deliverable 10 adds the call block
//! executor and the `FunctionExecutor` seam. Subsequent deliverables will add
//! transition scheduling, and the function-level and workflow-level drivers.

pub mod agent;
pub mod call;
pub mod prompt;

pub use agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
pub use call::{FunctionExecutor, execute_call_block};
pub use prompt::{ResolvedAgentConfig, execute_prompt_block, resolve_agent_config};
