//! mech stage -- placeholder that prints a one-line notice and returns no
//! tests. mech-cli's workflow engine cannot execute prompt or call blocks
//! against a real `AgentExecutor` yet (today it ships a `StubAgent` that
//! errors for any non-trivial workflow), so gate has nothing to assert
//! against. The stage exists so the orchestrator's stage enum is complete
//! and adding tests later is purely additive: when mech-cli grows real
//! workflow support, this module switches from `Vec::new()` to populated
//! tests without any other deliverable's wiring changing.

use crate::runner::StageContext;
use crate::types::TestResult;

pub fn run(_ctx: &StageContext) -> Vec<TestResult> {
    println!("mech: stage placeholder -- no tests defined yet");
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::BinaryPaths;
    use crate::types::GateConfig;
    use std::path::PathBuf;

    /// Spec TDD test #5: the mech placeholder returns an empty test
    /// vector. Pinned so a future contributor cannot silently slot in
    /// a stub test that always passes; mech tests must be deferred until
    /// mech-cli supports real workflow execution.
    ///
    /// `mech::run` ignores its context, so the inline `StageContext`
    /// is filled with placeholder paths -- the test pins the empty-vec
    /// contract, not any context-dependent behavior.
    #[test]
    fn mech_placeholder_returns_empty() {
        let dummy = PathBuf::from("dummy");
        let ctx = StageContext {
            binaries: BinaryPaths {
                flick: dummy.clone(),
                lot: dummy.clone(),
                reel: dummy.clone(),
                vault: dummy.clone(),
                epic: dummy.clone(),
                mech: dummy.clone(),
            },
            config: GateConfig {
                only: None,
                from: None,
                verbose: false,
                bin_dir: None,
                timeout: None,
                output_dir: dummy.clone(),
                keep_scratch: false,
            },
            scratch_dir: dummy.clone(),
            output_dir: dummy,
        };
        let results = run(&ctx);
        assert!(
            results.is_empty(),
            "mech stage must return zero TestResults until mech-cli grows real workflow support; got {} result(s)",
            results.len()
        );
    }
}
