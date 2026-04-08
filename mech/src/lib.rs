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
//! expression compiler/evaluator, and a JSON Schema registry with `$ref`
//! resolution and instance validation. There is no execution or runtime
//! logic yet — block scheduling, LLM dispatch, and load-time semantic
//! validation are still to come.

pub mod cel;
pub mod error;
pub mod schema;

pub use cel::{CelExpression, Namespaces, Template};
pub use error::{MechError, MechResult};
pub use schema::{
    AgentConfig, AgentConfigRef, BlockDef, CallBlock, CallEntry, CallSpec, CompactionConfig,
    ContextVarDef, FunctionDef, InferLiteral, ParallelStrategy, PromptBlock, ResolvedSchema,
    SchemaRef, SchemaRegistry, TransitionDef, WorkflowDefaults, WorkflowFile, parse_workflow,
};
