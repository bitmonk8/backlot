//! Runtime execution primitives.
//!
//! Deliverable 9 lands the prompt block executor and the `AgentExecutor` seam
//! used to inject a fake agent in tests. Deliverable 10 adds the call block
//! executor and the `FunctionExecutor` seam. Deliverable 11 adds transition
//! evaluation, `set_context`/`set_workflow` side-effects, and imperative-mode
//! function execution. Deliverable 12 adds the function executor, workflow
//! runtime, and dataflow scheduler.

pub mod agent;
pub mod call;
pub mod dataflow;
pub mod function;
pub mod prompt;
pub mod schedule;
pub mod system;
pub mod workflow;

#[cfg(test)]
pub(crate) mod test_support;

pub use agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
pub use call::{FunctionExecutor, execute_call_block};
pub use dataflow::run_function_dataflow;
pub use function::{ExecutionMode, FunctionRunner, detect_mode};
pub use prompt::{ResolvedAgentConfig, execute_prompt_block, resolve_agent_config};
pub use schedule::{
    TransitionResult, apply_side_effects, evaluate_transitions, run_function_imperative,
};
pub use workflow::WorkflowRuntime;
