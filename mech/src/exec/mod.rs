//! Runtime execution primitives.
//!
//! * [`prompt`] — prompt block executor, plus the [`AgentExecutor`] seam used
//!   to inject a fake agent in tests.
//! * [`call`] — call block executor, plus the [`FunctionExecutor`] seam.
//! * [`schedule`] — transition evaluation, `set_context`/`set_workflow`
//!   side-effects, imperative-mode function execution, and conversation
//!   scoping for prompt blocks.
//! * [`function`] — per-function executor.
//! * [`workflow`] — workflow-level runtime entry point.
//! * [`dataflow`] — dataflow-mode scheduler for functions wired only by
//!   `depends_on`.

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
