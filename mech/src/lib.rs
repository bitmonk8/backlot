//! # mech
//!
//! Declarative YAML-based workflow definition format targeting [`cue`] (task
//! orchestration) and [`reel`] (agent runtime).
//!
//! Mech workflows describe LLM-driven control- and dataflow as a unified CDFG
//! of prompt and call blocks, with CEL expressions for guards, templates, and
//! state mutations. See `docs/MECH_SPEC.md` for the full specification.
//!
//! This crate exposes: error types, parse-only serde schema types for the YAML
//! workflow grammar, a CEL expression compiler/evaluator, a JSON Schema
//! registry with `$ref` resolution and instance validation, a `validate`
//! module providing `validate_workflow` for §10.1 load-time checks, a `loader`
//! module exposing `WorkflowLoader::load(path) -> Workflow` which composes
//! parse → resolve schemas → validate → infer outputs → compile CEL into an
//! immutable `Send + Sync` [`Workflow`], an `exec` module holding prompt/call
//! block executors, transition evaluation, imperative-mode and dataflow-mode
//! function execution, [`FunctionRunner`] (recursive function dispatch with
//! depth limit), [`WorkflowRuntime`] (top-level entry point), and the
//! `AgentExecutor` / `FunctionExecutor` seams, plus per-invocation
//! `ExecutionContext` / shared `WorkflowState` types for runtime state.

pub mod cel;
pub mod context;
pub mod conversation;
pub mod cue_integration;
pub mod error;
pub mod exec;
pub mod loader;
pub mod schema;
pub mod validate;

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
pub use loader::{Workflow, WorkflowLoader};
pub use schema::{
    AgentConfig, AgentConfigRef, BlockDef, CallBlock, CallEntry, CallSpec, CompactionConfig,
    ContextVarDef, FunctionDef, InferLiteral, ParallelStrategy, PromptBlock, ResolvedSchema,
    SchemaRef, SchemaRegistry, TransitionDef, WorkflowDefaults, WorkflowFile,
    infer_function_outputs, parse_named_ref, parse_workflow, resolve_schema_value,
    try_parse_named_ref, value_matches_json_type,
};
pub use validate::{
    AnyModel, KnownModels, Location, ModelChecker, ValidationIssue, ValidationReport,
    validate_workflow,
};
