#![allow(dead_code)]
// Scaffolding: types and helpers are exercised by tests now and consumed by runner/stage modules added later.

//! Shared types used across gate modules.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

/// Test stages in dependency order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    Flick,
    Lot,
    Reel,
    Vault,
    Epic,
    Mech,
}

impl Stage {
    /// All stages in dependency order.
    pub fn all() -> [Stage; 6] {
        [
            Stage::Flick,
            Stage::Lot,
            Stage::Reel,
            Stage::Vault,
            Stage::Epic,
            Stage::Mech,
        ]
    }

    /// Per-stage default wall-clock timeout.
    pub fn default_timeout(self) -> Duration {
        match self {
            Stage::Epic => Duration::from_secs(600),
            _ => Duration::from_secs(300),
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Stage::Flick => "flick",
            Stage::Lot => "lot",
            Stage::Reel => "reel",
            Stage::Vault => "vault",
            Stage::Epic => "epic",
            Stage::Mech => "mech",
        };
        f.write_str(s)
    }
}

/// Error returned when parsing a stage from a string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStageError(pub String);

impl fmt::Display for ParseStageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown stage: {}", self.0)
    }
}

impl std::error::Error for ParseStageError {}

impl FromStr for Stage {
    type Err = ParseStageError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "flick" => Ok(Stage::Flick),
            "lot" => Ok(Stage::Lot),
            "reel" => Ok(Stage::Reel),
            "vault" => Ok(Stage::Vault),
            "epic" => Ok(Stage::Epic),
            "mech" => Ok(Stage::Mech),
            other => Err(ParseStageError(other.to_string())),
        }
    }
}

/// Outcome of a single test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// Test executed and verified all expectations.
    Pass,
    /// Hard failure: an assertion did not hold or the operation produced a
    /// wrong result. The string is a human-readable failure message and is
    /// surfaced in the summary table and JSON output.
    Fail(String),
    /// Test was not executed (e.g., platform unsupported, prerequisite
    /// missing). The string is a human-readable reason and is surfaced in
    /// the summary as the skip explanation.
    Skip(String),
    /// Soft failure: an infrastructure-dependent expectation did not hold
    /// (e.g., network reachability). Reported as a warning, never causes
    /// a non-zero exit. The string is a human-readable reason.
    SoftFail(String),
}

impl TestOutcome {
    pub fn is_pass(&self) -> bool {
        matches!(self, TestOutcome::Pass)
    }

    /// True only for hard failures. SoftFail does not count.
    pub fn is_failure(&self) -> bool {
        matches!(self, TestOutcome::Fail(_))
    }

    pub fn is_skip(&self) -> bool {
        matches!(self, TestOutcome::Skip(_))
    }

    pub fn is_soft_fail(&self) -> bool {
        matches!(self, TestOutcome::SoftFail(_))
    }
}

/// Result of a single test run.
#[derive(Debug, Clone)]
pub struct TestResult {
    pub stage: Stage,
    pub test: String,
    pub outcome: TestOutcome,
    pub duration: Duration,
    pub cost_usd: Option<f64>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// Captured subprocess stdout for this test, when the stage chose
    /// to record it. Used by the runner's `--verbose` mode to write
    /// `output/transcripts/{stage}-{test}.stdout`. `None` means "no
    /// transcript available"; the runner skips writing the file in that
    /// case rather than producing an empty placeholder.
    pub stdout: Option<String>,
    /// Captured subprocess stderr; same contract as `stdout`.
    pub stderr: Option<String>,
}

/// Aggregate results for one stage.
#[derive(Debug, Clone)]
pub struct StageResult {
    pub stage: Stage,
    pub results: Vec<TestResult>,
    pub duration: Duration,
}

impl StageResult {
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.outcome.is_pass()).count()
    }

    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome.is_failure())
            .count()
    }

    pub fn skipped(&self) -> usize {
        self.results.iter().filter(|r| r.outcome.is_skip()).count()
    }

    pub fn soft_failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome.is_soft_fail())
            .count()
    }

    pub fn total_cost(&self) -> f64 {
        self.results.iter().filter_map(|r| r.cost_usd).sum()
    }

    /// True if no hard failures. SoftFail does not count as failure.
    pub fn all_passed(&self) -> bool {
        self.failed() == 0
    }
}

/// Captured output of a subprocess invocation.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration: Duration,
}

/// Parsed run-time configuration for gate.
#[derive(Debug, Clone)]
pub struct GateConfig {
    pub only: Option<Stage>,
    pub from: Option<Stage>,
    pub verbose: bool,
    pub bin_dir: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub output_dir: PathBuf,
    pub keep_scratch: bool,
}

impl GateConfig {
    /// Returns explicit timeout if set, else the per-stage default.
    pub fn effective_timeout(&self, stage: Stage) -> Duration {
        self.timeout.unwrap_or_else(|| stage.default_timeout())
    }

    /// Whether the given stage should run under the current filters.
    pub fn should_run(&self, stage: Stage) -> bool {
        if let Some(only) = self.only {
            return stage == only;
        }
        if let Some(from) = self.from {
            return stage >= from;
        }
        true
    }

    /// True if scratch directories should be preserved on success.
    /// Verbose implies keep-scratch.
    pub fn effective_keep_scratch(&self) -> bool {
        self.keep_scratch || self.verbose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_result(outcome: TestOutcome, cost: Option<f64>) -> TestResult {
        TestResult {
            stage: Stage::Flick,
            test: "t".into(),
            outcome,
            duration: Duration::from_secs(0),
            cost_usd: cost,
            tokens_in: None,
            tokens_out: None,
            stdout: None,
            stderr: None,
        }
    }

    fn default_config() -> GateConfig {
        GateConfig {
            only: None,
            from: None,
            verbose: false,
            bin_dir: None,
            timeout: None,
            output_dir: PathBuf::from("gate/output/"),
            keep_scratch: false,
        }
    }

    #[test]
    fn stage_display() {
        assert_eq!(Stage::Flick.to_string(), "flick");
        assert_eq!(Stage::Lot.to_string(), "lot");
        assert_eq!(Stage::Reel.to_string(), "reel");
        assert_eq!(Stage::Vault.to_string(), "vault");
        assert_eq!(Stage::Epic.to_string(), "epic");
        assert_eq!(Stage::Mech.to_string(), "mech");
    }

    #[test]
    fn stage_from_str_valid() {
        assert_eq!("flick".parse::<Stage>().unwrap(), Stage::Flick);
        assert_eq!("lot".parse::<Stage>().unwrap(), Stage::Lot);
        assert_eq!("reel".parse::<Stage>().unwrap(), Stage::Reel);
        assert_eq!("vault".parse::<Stage>().unwrap(), Stage::Vault);
        assert_eq!("epic".parse::<Stage>().unwrap(), Stage::Epic);
        assert_eq!("mech".parse::<Stage>().unwrap(), Stage::Mech);
    }

    #[test]
    fn stage_from_str_invalid() {
        assert!("unknown".parse::<Stage>().is_err());
        assert!("".parse::<Stage>().is_err());
        assert!("FLICK".parse::<Stage>().is_err());
    }

    #[test]
    fn stage_ordering() {
        assert_eq!(
            Stage::all(),
            [
                Stage::Flick,
                Stage::Lot,
                Stage::Reel,
                Stage::Vault,
                Stage::Epic,
                Stage::Mech,
            ]
        );
    }

    #[test]
    fn stage_default_timeout() {
        assert_eq!(Stage::Epic.default_timeout(), Duration::from_secs(600));
        for s in [
            Stage::Flick,
            Stage::Lot,
            Stage::Reel,
            Stage::Vault,
            Stage::Mech,
        ] {
            assert_eq!(s.default_timeout(), Duration::from_secs(300));
        }
    }

    #[test]
    fn test_outcome_classification() {
        assert!(TestOutcome::Pass.is_pass());
        assert!(!TestOutcome::Pass.is_failure());

        let f = TestOutcome::Fail("x".into());
        assert!(f.is_failure());
        assert!(!f.is_pass());

        let s = TestOutcome::Skip("x".into());
        assert!(s.is_skip());
        assert!(!s.is_failure());

        let sf = TestOutcome::SoftFail("x".into());
        assert!(sf.is_soft_fail());
        assert!(!sf.is_failure());
    }

    #[test]
    fn stage_result_counts() {
        let sr = StageResult {
            stage: Stage::Flick,
            results: vec![
                mk_result(TestOutcome::Pass, None),
                mk_result(TestOutcome::Pass, None),
                mk_result(TestOutcome::Fail("x".into()), None),
                mk_result(TestOutcome::Skip("x".into()), None),
                mk_result(TestOutcome::SoftFail("x".into()), None),
            ],
            duration: Duration::from_secs(1),
        };
        assert_eq!(sr.passed(), 2);
        assert_eq!(sr.failed(), 1);
        assert_eq!(sr.skipped(), 1);
        assert_eq!(sr.soft_failed(), 1);
    }

    #[test]
    fn stage_result_all_passed() {
        let sr_ok = StageResult {
            stage: Stage::Flick,
            results: vec![
                mk_result(TestOutcome::Pass, None),
                mk_result(TestOutcome::SoftFail("x".into()), None),
            ],
            duration: Duration::from_secs(0),
        };
        assert!(sr_ok.all_passed());

        let sr_bad = StageResult {
            stage: Stage::Flick,
            results: vec![mk_result(TestOutcome::Fail("x".into()), None)],
            duration: Duration::from_secs(0),
        };
        assert!(!sr_bad.all_passed());
    }

    #[test]
    fn stage_result_total_cost() {
        let sr = StageResult {
            stage: Stage::Flick,
            results: vec![
                mk_result(TestOutcome::Pass, Some(0.10)),
                mk_result(TestOutcome::Pass, Some(0.05)),
                mk_result(TestOutcome::Pass, None),
            ],
            duration: Duration::from_secs(0),
        };
        assert!((sr.total_cost() - 0.15).abs() < 1e-9);
    }

    #[test]
    fn gate_config_should_run_no_filter() {
        let cfg = default_config();
        for s in Stage::all() {
            assert!(cfg.should_run(s));
        }
    }

    #[test]
    fn gate_config_should_run_only() {
        let mut cfg = default_config();
        cfg.only = Some(Stage::Reel);
        for s in Stage::all() {
            assert_eq!(cfg.should_run(s), s == Stage::Reel);
        }
    }

    #[test]
    fn gate_config_should_run_from() {
        let mut cfg = default_config();
        cfg.from = Some(Stage::Vault);
        assert!(!cfg.should_run(Stage::Flick));
        assert!(!cfg.should_run(Stage::Lot));
        assert!(!cfg.should_run(Stage::Reel));
        assert!(cfg.should_run(Stage::Vault));
        assert!(cfg.should_run(Stage::Epic));
        assert!(cfg.should_run(Stage::Mech));
    }

    #[test]
    fn gate_config_effective_timeout_explicit() {
        let mut cfg = default_config();
        cfg.timeout = Some(Duration::from_secs(42));
        assert_eq!(cfg.effective_timeout(Stage::Flick), Duration::from_secs(42));
        assert_eq!(cfg.effective_timeout(Stage::Epic), Duration::from_secs(42));
    }

    #[test]
    fn gate_config_effective_timeout_default() {
        let cfg = default_config();
        assert_eq!(
            cfg.effective_timeout(Stage::Flick),
            Duration::from_secs(300)
        );
        assert_eq!(cfg.effective_timeout(Stage::Epic), Duration::from_secs(600));
    }

    #[test]
    fn gate_config_effective_keep_scratch() {
        let mut cfg = default_config();
        assert!(!cfg.effective_keep_scratch());
        cfg.verbose = true;
        cfg.keep_scratch = false;
        assert!(cfg.effective_keep_scratch());
        cfg.verbose = false;
        cfg.keep_scratch = true;
        assert!(cfg.effective_keep_scratch());
    }
}
