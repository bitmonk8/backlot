//! # mech
//!
//! Declarative YAML-based workflow definition format targeting [`cue`] (task
//! orchestration) and [`reel`] (agent runtime).
//!
//! Mech workflows describe LLM-driven control- and dataflow as a unified CDFG
//! of prompt and call blocks, with CEL expressions for guards, templates, and
//! state mutations. See `docs/MECH_SPEC.md` for the full specification.
//!
//! This crate is under active TDD development. It currently exposes error
//! types, parse-only serde schema types for the YAML workflow grammar, a CEL
//! expression compiler/evaluator, a JSON Schema registry with `$ref`
//! resolution and instance validation, a `validate` module providing
//! `validate_workflow(&WorkflowFile, Option<&Path>, &dyn ModelChecker) ->
//! ValidationReport` which performs the §10.1 single-pass load-time checks,
//! and a `loader` module exposing `WorkflowLoader::load(path) -> Workflow`
//! which composes parse → resolve schemas → validate → infer outputs →
//! compile CEL into an immutable, `Send + Sync` [`Workflow`] value ready for
//! execution, an `exec` module holding the prompt block executor and the
//! `AgentExecutor` seam used to inject the agent runtime (reel in
//! production once wired alongside the workflow driver, a fake in tests), and per-invocation `ExecutionContext` /
//! shared `WorkflowState` types for runtime state. Block scheduling,
//! transitions, and the function / workflow drivers are still to come.

pub mod cel;
pub mod context;
pub mod error;
pub mod exec;
pub mod loader;
pub mod schema;
pub mod validate;

pub use cel::{CelExpression, Namespaces, Template};
pub use context::{ExecutionContext, WorkflowState};
pub use error::{MechError, MechResult};
pub use loader::{Workflow, WorkflowLoader};
pub use schema::{
    AgentConfig, AgentConfigRef, BlockDef, CallBlock, CallEntry, CallSpec, CompactionConfig,
    ContextVarDef, FunctionDef, InferLiteral, ParallelStrategy, PromptBlock, ResolvedSchema,
    SchemaRef, SchemaRegistry, TransitionDef, WorkflowDefaults, WorkflowFile,
    infer_function_outputs, parse_workflow,
};
pub use validate::{
    AnyModel, KnownModels, Location, ModelChecker, ValidationIssue, ValidationReport,
    validate_workflow,
};
