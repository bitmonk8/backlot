//! # mech
//!
//! Declarative YAML-based workflow definition format targeting [`cue`] (task
//! orchestration) and [`reel`] (agent runtime).
//!
//! Mech workflows describe LLM-driven control- and dataflow as a unified CDFG
//! of prompt and call blocks, with CEL expressions for guards, templates, and
//! state mutations. See `docs/MECH_SPEC.md` for the full specification.
//!
//! This crate is under active TDD development. Deliverable 1 (this module)
//! provides only the crate skeleton and error surface — no execution logic.

pub mod error;

pub use error::{MechError, MechResult};
