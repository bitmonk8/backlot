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
//! resolution and instance validation, and a `validate` module providing
//! `validate_workflow(&WorkflowFile, Option<&Path>, &dyn ModelChecker) ->
//! ValidationReport` which performs the §10.1 single-pass load-time checks.
//! There is no execution or runtime logic yet — block scheduling and LLM
//! dispatch are still to come.

pub mod cel;
pub mod error;
pub mod schema;
pub mod validate;

pub use cel::{CelExpression, Namespaces, Template};
pub use error::{MechError, MechResult};
pub use schema::{
    AgentConfig, AgentConfigRef, BlockDef, CallBlock, CallEntry, CallSpec, CompactionConfig,
    ContextVarDef, FunctionDef, InferLiteral, ParallelStrategy, PromptBlock, ResolvedSchema,
    SchemaRef, SchemaRegistry, TransitionDef, WorkflowDefaults, WorkflowFile, parse_workflow,
};
pub use validate::{
    AnyModel, KnownModels, Location, ModelChecker, ValidationIssue, ValidationReport,
    validate_workflow,
};
