//! Stage implementations.
//!
//! Each submodule provides `pub fn run(ctx: &StageContext) -> Vec<TestResult>`
//! matching the [`crate::runner::StageFn`] signature. Modules that are
//! still empty stubs return `Vec::new()`; their bodies will be filled in
//! as each stage is wired to its real subprocess tests.

pub mod epic;
pub mod flick;
pub mod lot;
pub mod mech;
pub mod reel;
pub mod vault;
