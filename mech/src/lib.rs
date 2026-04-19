//! Declarative YAML-based workflow definition and execution engine.
//!
//! Mech workflows describe LLM-driven control and dataflow as prompt and call
//! blocks with CEL guards, template expressions, and declared state variables.
//! See `docs/MECH_SPEC.md` for the full specification.

pub mod cel;
pub mod context;
pub mod conversation;
pub mod cue_integration;
pub mod error;
pub mod exec;
pub mod loader;
pub mod schema;
pub mod validate;
pub mod workflow;

pub use cel::{CelExpression, Namespaces, Template, cel_value_to_json};
pub use context::{ExecutionContext, WorkflowState};
pub use conversation::{Conversation, Message, ResolvedCompaction, Role, resolve_compaction};
pub use cue_integration::{MechStore, MechTask};
pub use error::{MechError, MechResult};
pub use exec::call::FunctionExecutor;
pub use exec::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
pub use exec::{
    ExecutionMode, FunctionRunner, WorkflowRuntime, detect_mode, run_function_dataflow,
};
pub use exec::{
    ResolvedAgentConfig, execute_call_block, execute_prompt_block, resolve_agent_config,
};
pub use exec::{
    TransitionResult, apply_side_effects, evaluate_transitions, run_function_imperative,
};
pub use loader::{
    UnsupportedFeatureAdvisory, WorkflowLoader, collect_unsupported_feature_advisories,
    load_workflow, load_workflow_str, load_workflow_str_with, load_workflow_with,
};
pub use schema::{
    AgentConfig, AgentConfigRef, BlockDef, CallBlock, CallEntry, CallSpec, CompactionConfig,
    ContextVarDef, ExecutionConfig, FunctionDef, MechDocument, ParallelStrategy, PromptBlock,
    ResolvedSchema, SchemaRef, SchemaRegistry, TransitionDef, WorkflowSection,
    infer_function_outputs, parse_named_ref, parse_workflow, resolve_schema_ref_in_map,
    resolve_schema_value, try_parse_named_ref, value_matches_json_type,
};
pub use validate::{
    AnyModel, KnownModels, Location, ModelChecker, ValidationIssue, ValidationReport,
    validate_workflow,
};
pub use workflow::Workflow;
