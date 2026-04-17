#![allow(dead_code)]
// Scaffolding: BinaryPaths::for_stage and StageContext fields will be
// consumed by the stage modules added in D6-D8. Remove this allow once
// those modules wire them in.

//! Binary discovery and stage orchestration.
//!
//! [`discover_binaries`] resolves the `flick`/`lot`/`reel`/`vault`/`epic`/`mech`
//! executables under either an explicit `--bin-dir` or the workspace
//! `target/<profile>/` directory derived from gate's own running executable.
//! [`run`] is the single entry point invoked from `main`: it discovers
//! binaries, creates the per-run scratch tree, dispatches each filtered
//! stage, prints the summary, and decides the process exit code.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::report;
use crate::scratch;
use crate::types::{GateConfig, Stage, StageResult, TestResult};

/// Resolved on-disk paths to every backlot binary gate orchestrates.
#[derive(Debug, Clone)]
pub struct BinaryPaths {
    pub flick: PathBuf,
    pub lot: PathBuf,
    pub reel: PathBuf,
    pub vault: PathBuf,
    pub epic: PathBuf,
    pub mech: PathBuf,
}

impl BinaryPaths {
    /// Look up the resolved path for a given stage's binary.
    pub fn for_stage(&self, stage: Stage) -> &Path {
        match stage {
            Stage::Flick => &self.flick,
            Stage::Lot => &self.lot,
            Stage::Reel => &self.reel,
            Stage::Vault => &self.vault,
            Stage::Epic => &self.epic,
            Stage::Mech => &self.mech,
        }
    }
}

/// Returned by [`discover_binaries`] when one or more required binaries
/// are absent from the search directory.
///
/// The error names *every* missing binary (not just the first one), so a
/// single `cargo build` covers the user's whole gap rather than forcing
/// them through a discover-build-rerun loop per missing crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryError {
    /// Filenames as they appear on disk (so `flick.exe` on Windows,
    /// `flick` on Unix). Order matches [`Stage::all`].
    pub missing: Vec<String>,
    /// The directory that was searched.
    pub search_dir: PathBuf,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "missing backlot binaries in {}: {}",
            self.search_dir.display(),
            self.missing.join(", ")
        )
    }
}

impl std::error::Error for DiscoveryError {}

/// Produce the on-disk filename for a backlot binary on the current
/// platform (appends `.exe` on Windows, leaves it bare on Unix).
pub(crate) fn binary_filename(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Locate every required binary.
///
/// Resolution order:
/// 1. `bin_dir` if provided -- used verbatim, no further fallback.
/// 2. The directory containing gate's own executable
///    (`std::env::current_exe()`'s parent). When `current_exe()` lives in
///    a `deps/` subdirectory (the layout `cargo test` and similar
///    harnesses use), the grandparent is returned so workspace
///    `target/<profile>/` discovery still works.
/// 3. `CARGO_MANIFEST_DIR/../target/debug` as a last-resort fallback when
///    `current_exe` is unavailable (rare; some embedded runtimes).
pub fn discover_binaries(bin_dir: Option<&Path>) -> Result<BinaryPaths, DiscoveryError> {
    let search_dir = bin_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(default_bin_dir);

    let mut found: [Option<PathBuf>; 6] = Default::default();
    let mut missing: Vec<String> = Vec::new();
    for (idx, stage) in Stage::all().iter().enumerate() {
        let fname = binary_filename(&stage.to_string());
        let candidate = search_dir.join(&fname);
        if candidate.is_file() {
            found[idx] = Some(candidate);
        } else {
            missing.push(fname);
        }
    }

    if !missing.is_empty() {
        return Err(DiscoveryError {
            missing,
            search_dir,
        });
    }

    // Every slot was populated; the unwraps are infallible here.
    let mut take = |i: usize| -> PathBuf { found[i].take().expect("slot populated above") };
    Ok(BinaryPaths {
        flick: take(0),
        lot: take(1),
        reel: take(2),
        vault: take(3),
        epic: take(4),
        mech: take(5),
    })
}

/// Default search directory: the parent of gate's own executable. When
/// `current_exe()` lives in a `deps/` subdirectory (the layout `cargo
/// test` and similar harnesses use), the grandparent is returned so
/// workspace `target/<profile>/` discovery still works. Falls back to
/// `<workspace>/target/debug` if `current_exe()` is unavailable.
fn default_bin_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        if parent.file_name().and_then(|n| n.to_str()) == Some("deps")
            && let Some(grand) = parent.parent()
            && grand.file_name().is_some()
        {
            return grand.to_path_buf();
        }
        return parent.to_path_buf();
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|w| w.join("target").join("debug"))
        .unwrap_or_else(|| PathBuf::from("target/debug"))
}

/// Signature each stage module exposes through its `pub fn run(...)`.
/// Plain function pointer rather than a trait so the registry below is a
/// trivial `match`, not a vtable lookup.
pub type StageFn = fn(ctx: &StageContext) -> Vec<TestResult>;

/// Per-stage execution context handed to every stage function.
///
/// `binaries` and `config` are cloned per stage so a stage that wants to
/// mutate either can do so without affecting siblings -- none currently do,
/// but the contract is cheaper to honor up front than to retrofit.
#[derive(Debug, Clone)]
pub struct StageContext {
    pub binaries: BinaryPaths,
    pub config: GateConfig,
    /// Per-stage subdirectory within the per-run scratch directory
    /// (e.g., `target/gate-scratch/run-X/lot/`). Always exists when the
    /// stage is invoked.
    pub scratch_dir: PathBuf,
    /// Where `--verbose` writes the run's `results.json`. Reserved for
    /// per-test transcripts in a later deliverable; until then, only
    /// `results.json` lands here.
    pub output_dir: PathBuf,
}

/// Run all stages permitted by `config`, print the summary, write
/// `results.json` (when verbose), and clean up the scratch tree.
///
/// Returns the process exit code:
/// * `0` -- every executed stage passed (or only soft-failed)
/// * `1` -- at least one hard `Fail`
/// * `2` -- prerequisite failure (binary discovery, scratch creation, or
///   `--verbose` output-write failure)
pub fn run(config: GateConfig) -> i32 {
    let binaries = match discover_binaries(config.bin_dir.as_deref()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("gate: {e}");
            eprintln!("gate: try `cargo build` from the workspace root");
            return 2;
        }
    };
    let run_dir = match scratch::create_run_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("gate: failed to create scratch dir: {e}");
            return 2;
        }
    };

    let (code, _results, _summary) = run_inner(config, binaries, run_dir, |stage, ctx| {
        dispatch_stage(stage)(ctx)
    });
    code
}

/// Default registry: the production stage-fn for each stage.
fn dispatch_stage(stage: Stage) -> StageFn {
    match stage {
        Stage::Flick => crate::stage::flick::run,
        Stage::Lot => crate::stage::lot::run,
        Stage::Reel => crate::stage::reel::run,
        Stage::Vault => crate::stage::vault::run,
        Stage::Epic => crate::stage::epic::run,
        Stage::Mech => crate::stage::mech::run,
    }
}

/// Inner orchestration loop, parameterized over the stage runner so tests
/// can inject closures without depending on real binary execution.
///
/// Returns `(exit_code, stage_results, formatted_summary)`. `run` discards
/// the latter two; tests inspect them.
fn run_inner<F>(
    config: GateConfig,
    binaries: BinaryPaths,
    run_dir: PathBuf,
    mut stage_runner: F,
) -> (i32, Vec<StageResult>, String)
where
    F: FnMut(Stage, &StageContext) -> Vec<TestResult>,
{
    let mut stage_results: Vec<StageResult> = Vec::new();

    for stage in Stage::all() {
        if !config.should_run(stage) {
            continue;
        }
        let scratch_dir = run_dir.join(stage.to_string());
        // Per-stage subdir is normally pre-created by `create_run_dir`
        // for the lot/reel/vault/epic stages, but flick and mech are not
        // pre-populated. Create defensively so every stage observes its
        // own existing scratch path regardless of which stages produced it.
        if let Err(e) = std::fs::create_dir_all(&scratch_dir) {
            eprintln!(
                "gate: failed to create scratch subdir {}: {}",
                scratch_dir.display(),
                e
            );
            stage_results.push(StageResult {
                stage,
                results: vec![TestResult {
                    stage,
                    test: "gate:scratch-setup".into(),
                    outcome: crate::types::TestOutcome::Fail(format!(
                        "could not create scratch dir {}: {}",
                        scratch_dir.display(),
                        e
                    )),
                    duration: std::time::Duration::ZERO,
                    cost_usd: None,
                    tokens_in: None,
                    tokens_out: None,
                }],
                duration: std::time::Duration::ZERO,
            });
            continue;
        }

        let ctx = StageContext {
            binaries: binaries.clone(),
            config: config.clone(),
            scratch_dir,
            output_dir: config.output_dir.clone(),
        };
        let start = Instant::now();
        let results = stage_runner(stage, &ctx);
        let duration = start.elapsed();
        stage_results.push(StageResult {
            stage,
            results,
            duration,
        });
    }

    let summary = report::format_summary(&stage_results);
    print!("{summary}");

    let any_failure = stage_results.iter().any(|sr| !sr.all_passed());

    let mut verbose_io_failed = false;
    if config.verbose {
        if let Err(e) = std::fs::create_dir_all(&config.output_dir) {
            eprintln!(
                "gate: --verbose requested but failed to create output dir {}: {}",
                config.output_dir.display(),
                e
            );
            eprintln!("gate: run will exit non-zero");
            verbose_io_failed = true;
        } else {
            let json_path = config.output_dir.join("results.json");
            if let Err(e) = report::write_results_json(&stage_results, &json_path) {
                eprintln!(
                    "gate: --verbose requested but failed to write {}: {}",
                    json_path.display(),
                    e
                );
                eprintln!("gate: run will exit non-zero");
                verbose_io_failed = true;
            }
        }
    }

    if !config.effective_keep_scratch() && !any_failure {
        if let Err(e) = scratch::cleanup_run_dir(&run_dir) {
            eprintln!(
                "gate: scratch cleanup failed at {}: {}",
                run_dir.display(),
                e
            );
        }
    } else {
        eprintln!("gate: scratch preserved at {}", run_dir.display());
    }

    let code = if verbose_io_failed && !any_failure {
        2
    } else if any_failure {
        1
    } else {
        0
    };
    (code, stage_results, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::scratch_base;
    use crate::types::TestOutcome;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Project-local scratch dir for one test. Lives under
    /// `target/gate-scratch/runner-tests/` to honor the workspace
    /// CLAUDE.md rule against system temp.
    struct TestDir(PathBuf);
    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = scratch_base()
                .join("runner-tests")
                .join(format!("{label}-{pid}-{id}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, b"").expect("touch file");
    }

    fn touch_binaries(dir: &Path, names: &[&str]) {
        for n in names {
            touch(&dir.join(binary_filename(n)));
        }
    }

    fn dummy_binaries(dir: &Path) -> BinaryPaths {
        BinaryPaths {
            flick: dir.join(binary_filename("flick")),
            lot: dir.join(binary_filename("lot")),
            reel: dir.join(binary_filename("reel")),
            vault: dir.join(binary_filename("vault")),
            epic: dir.join(binary_filename("epic")),
            mech: dir.join(binary_filename("mech")),
        }
    }

    fn default_test_config() -> GateConfig {
        GateConfig {
            only: None,
            from: None,
            verbose: false,
            bin_dir: None,
            timeout: None,
            output_dir: PathBuf::from("gate/output/"),
            // Force-keep so cleanup races between parallel tests do not
            // tear down a sibling test's run dir; tests inspect the tree
            // after run_inner returns.
            keep_scratch: true,
        }
    }

    fn pass_result(stage: Stage, name: &str) -> TestResult {
        TestResult {
            stage,
            test: name.into(),
            outcome: TestOutcome::Pass,
            duration: Duration::ZERO,
            cost_usd: None,
            tokens_in: None,
            tokens_out: None,
        }
    }

    fn fail_result(stage: Stage, name: &str) -> TestResult {
        TestResult {
            stage,
            test: name.into(),
            outcome: TestOutcome::Fail("intentional".into()),
            duration: Duration::ZERO,
            cost_usd: None,
            tokens_in: None,
            tokens_out: None,
        }
    }

    fn soft_fail_result(stage: Stage, name: &str) -> TestResult {
        TestResult {
            stage,
            test: name.into(),
            outcome: TestOutcome::SoftFail("net".into()),
            duration: Duration::ZERO,
            cost_usd: None,
            tokens_in: None,
            tokens_out: None,
        }
    }

    fn make_run_dir() -> (PathBuf, TestDir) {
        let td = TestDir::new("run-dir");
        let run = td.path().join("run-x");
        for sub in ["lot", "reel", "vault", "epic"] {
            fs::create_dir_all(run.join(sub)).expect("create subdir");
        }
        (run, td)
    }

    // ---- Binary discovery tests ----

    #[test]
    fn discover_explicit_dir_all_present() {
        let dir = TestDir::new("disc-all");
        touch_binaries(
            dir.path(),
            &["flick", "lot", "reel", "vault", "epic", "mech"],
        );
        let bp = discover_binaries(Some(dir.path())).expect("all present");
        assert_eq!(bp.flick, dir.path().join(binary_filename("flick")));
        assert_eq!(bp.lot, dir.path().join(binary_filename("lot")));
        assert_eq!(bp.reel, dir.path().join(binary_filename("reel")));
        assert_eq!(bp.vault, dir.path().join(binary_filename("vault")));
        assert_eq!(bp.epic, dir.path().join(binary_filename("epic")));
        assert_eq!(bp.mech, dir.path().join(binary_filename("mech")));
        assert_eq!(bp.for_stage(Stage::Flick), bp.flick.as_path());
        assert_eq!(bp.for_stage(Stage::Mech), bp.mech.as_path());
    }

    #[test]
    fn discover_explicit_dir_missing_some() {
        let dir = TestDir::new("disc-some");
        touch_binaries(dir.path(), &["flick", "lot", "reel"]);
        let err = discover_binaries(Some(dir.path())).expect_err("some missing");
        assert_eq!(err.missing.len(), 3, "missing was: {:?}", err.missing);
        for needed in ["vault", "epic", "mech"] {
            let want = binary_filename(needed);
            assert!(
                err.missing.contains(&want),
                "expected {want} in {:?}",
                err.missing
            );
        }
        for present in ["flick", "lot", "reel"] {
            let want = binary_filename(present);
            assert!(
                !err.missing.contains(&want),
                "did not expect {want} in {:?}",
                err.missing
            );
        }
    }

    #[test]
    fn discover_explicit_dir_empty() {
        let dir = TestDir::new("disc-empty");
        let err = discover_binaries(Some(dir.path())).expect_err("all missing");
        assert_eq!(err.missing.len(), 6, "missing was: {:?}", err.missing);
        for n in ["flick", "lot", "reel", "vault", "epic", "mech"] {
            assert!(
                err.missing.contains(&binary_filename(n)),
                "expected {n} in {:?}",
                err.missing
            );
        }
    }

    #[test]
    fn discover_reports_search_dir() {
        let dir = TestDir::new("disc-search-dir");
        let err = discover_binaries(Some(dir.path())).expect_err("missing");
        assert_eq!(err.search_dir, dir.path());
        let msg = format!("{err}");
        assert!(
            msg.contains(&dir.path().display().to_string()),
            "expected search dir in display, got: {msg}"
        );
    }

    #[test]
    fn binary_names_platform_suffix() {
        let want_flick = if cfg!(windows) { "flick.exe" } else { "flick" };
        let want_mech = if cfg!(windows) { "mech.exe" } else { "mech" };
        assert_eq!(binary_filename("flick"), want_flick);
        assert_eq!(binary_filename("mech"), want_mech);

        let dir = TestDir::new("disc-suffix");
        let err = discover_binaries(Some(dir.path())).expect_err("missing");
        assert!(
            err.missing.iter().all(|n| {
                if cfg!(windows) {
                    n.ends_with(".exe")
                } else {
                    !n.ends_with(".exe")
                }
            }),
            "platform suffix incorrect in: {:?}",
            err.missing
        );
    }

    // ---- Stage filtering tests ----

    #[test]
    fn run_all_stages_no_filter() {
        let dir = TestDir::new("filter-all");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let calls = Arc::new(Mutex::new(Vec::<Stage>::new()));
        let calls_for_closure = Arc::clone(&calls);
        let (_code, results, _summary) = run_inner(cfg, binaries, run_dir, |s, _ctx| {
            calls_for_closure.lock().expect("mutex").push(s);
            vec![pass_result(s, "stub")]
        });
        let stages_called: Vec<Stage> = calls.lock().expect("mutex").clone();
        assert_eq!(stages_called.len(), 6, "called: {stages_called:?}");
        assert_eq!(results.len(), 6);
    }

    #[test]
    fn run_only_one_stage() {
        let dir = TestDir::new("filter-only");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let mut cfg = default_test_config();
        cfg.only = Some(Stage::Reel);
        let calls = Arc::new(Mutex::new(Vec::<Stage>::new()));
        let calls_for_closure = Arc::clone(&calls);
        let (_code, results, _summary) = run_inner(cfg, binaries, run_dir, |s, _ctx| {
            calls_for_closure.lock().expect("mutex").push(s);
            vec![pass_result(s, "stub")]
        });
        let stages_called: Vec<Stage> = calls.lock().expect("mutex").clone();
        assert_eq!(stages_called, vec![Stage::Reel]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].stage, Stage::Reel);
    }

    #[test]
    fn run_from_stage() {
        let dir = TestDir::new("filter-from");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let mut cfg = default_test_config();
        cfg.from = Some(Stage::Reel);
        let calls = Arc::new(Mutex::new(Vec::<Stage>::new()));
        let calls_for_closure = Arc::clone(&calls);
        let (_code, results, _summary) = run_inner(cfg, binaries, run_dir, |s, _ctx| {
            calls_for_closure.lock().expect("mutex").push(s);
            vec![pass_result(s, "stub")]
        });
        let stages_called: Vec<Stage> = calls.lock().expect("mutex").clone();
        assert_eq!(
            stages_called,
            vec![Stage::Reel, Stage::Vault, Stage::Epic, Stage::Mech]
        );
        assert_eq!(results.len(), 4);
    }

    #[test]
    fn stages_run_in_order() {
        let dir = TestDir::new("filter-order");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let calls = Arc::new(Mutex::new(Vec::<Stage>::new()));
        let calls_for_closure = Arc::clone(&calls);
        let (_code, results, _summary) = run_inner(cfg, binaries, run_dir, |s, _ctx| {
            calls_for_closure.lock().expect("mutex").push(s);
            vec![pass_result(s, "stub")]
        });
        let stages_called: Vec<Stage> = calls.lock().expect("mutex").clone();
        assert_eq!(stages_called, Stage::all().to_vec());
        let result_stages: Vec<Stage> = results.iter().map(|r| r.stage).collect();
        assert_eq!(result_stages, Stage::all().to_vec());
    }

    #[test]
    fn stage_context_has_correct_scratch_dir() {
        let dir = TestDir::new("ctx-scratch");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let captured: Arc<Mutex<Vec<(Stage, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_closure = Arc::clone(&captured);
        let _ = run_inner(cfg, binaries, run_dir.clone(), |s, ctx| {
            captured_for_closure
                .lock()
                .expect("mutex")
                .push((s, ctx.scratch_dir.clone()));
            assert!(
                ctx.scratch_dir.is_dir(),
                "scratch dir {} should exist when stage runs",
                ctx.scratch_dir.display()
            );
            vec![pass_result(s, "stub")]
        });
        let recorded = captured.lock().expect("mutex").clone();
        assert_eq!(recorded.len(), 6);
        for (stage, scratch) in recorded {
            assert_eq!(
                scratch,
                run_dir.join(stage.to_string()),
                "stage {stage} got the wrong scratch dir"
            );
        }
    }

    // ---- Exit code tests ----

    #[test]
    fn exit_0_when_all_pass() {
        let dir = TestDir::new("exit-pass");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let (code, _results, _summary) =
            run_inner(cfg, binaries, run_dir, |s, _ctx| vec![pass_result(s, "p")]);
        assert_eq!(code, 0);
    }

    #[test]
    fn exit_1_when_any_fail() {
        let dir = TestDir::new("exit-fail");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let calls = Arc::new(Mutex::new(Vec::<Stage>::new()));
        let calls_for_closure = Arc::clone(&calls);
        let (code, _results, _summary) = run_inner(cfg, binaries, run_dir, |s, _ctx| {
            calls_for_closure.lock().expect("mutex").push(s);
            if s == Stage::Reel {
                vec![fail_result(s, "boom")]
            } else {
                vec![pass_result(s, "p")]
            }
        });
        assert_eq!(code, 1);
        // Don't-abort contract: a hard Fail in Reel must not short-circuit
        // the loop; every later stage still has to run so the operator
        // sees the full picture.
        let stages_called: Vec<Stage> = calls.lock().expect("mutex").clone();
        assert_eq!(stages_called, Stage::all().to_vec());
    }

    #[test]
    fn exit_0_when_soft_fail_only() {
        let dir = TestDir::new("exit-soft");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let (code, _results, _summary) = run_inner(cfg, binaries, run_dir, |s, _ctx| {
            vec![pass_result(s, "p"), soft_fail_result(s, "net")]
        });
        assert_eq!(code, 0);
    }

    // ---- Reporting integration ----

    #[test]
    fn summary_generated() {
        let dir = TestDir::new("summary");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let cfg = default_test_config();
        let (_code, results, summary) =
            run_inner(cfg, binaries, run_dir, |s, _ctx| vec![pass_result(s, "p")]);
        assert_eq!(results.len(), 6);
        assert!(
            summary.contains("Gate"),
            "expected 'Gate' in summary: {summary}"
        );
        assert!(
            summary.contains("Total"),
            "expected 'Total' row in summary: {summary}"
        );
        for stage in Stage::all() {
            assert!(
                summary.contains(&stage.to_string()),
                "expected stage {stage} in summary: {summary}"
            );
        }
    }

    // ---- Synthetic scratch-setup failure ----

    #[test]
    fn scratch_subdir_failure_yields_synthetic_fail() {
        // Use a regular file as run_dir so `run_dir.join(stage)` lands
        // under a non-directory ancestor; `create_dir_all` then fails
        // deterministically without depending on permissions.
        let td = TestDir::new("scratch-fail");
        let not_a_dir = td.path().join("not-a-dir");
        fs::write(&not_a_dir, b"sentinel").expect("write file");

        let dir = TestDir::new("scratch-fail-bin");
        let binaries = dummy_binaries(dir.path());
        let mut cfg = default_test_config();
        cfg.only = Some(Stage::Flick);

        let (code, results, _summary) = run_inner(cfg, binaries, not_a_dir, |_s, _ctx| {
            panic!("stage_runner must not be invoked when scratch creation fails")
        });

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].results.len(), 1);
        assert_eq!(results[0].results[0].test, "gate:scratch-setup");
        assert!(
            matches!(results[0].results[0].outcome, TestOutcome::Fail(_)),
            "expected Fail, got {:?}",
            results[0].results[0].outcome
        );
        assert_eq!(code, 1);
    }

    // ---- Cleanup runs on success when keep_scratch=false ----

    #[test]
    fn cleanup_runs_on_success_without_keep_scratch() {
        let dir = TestDir::new("cleanup-success-bin");
        let binaries = dummy_binaries(dir.path());
        let (run_dir, _td) = make_run_dir();
        let mut cfg = default_test_config();
        cfg.keep_scratch = false;
        cfg.verbose = false;

        assert!(run_dir.exists(), "precondition: run_dir should exist");
        let (code, _results, _summary) = run_inner(cfg, binaries, run_dir.clone(), |s, _ctx| {
            vec![pass_result(s, "p")]
        });
        assert_eq!(code, 0);
        assert!(
            !run_dir.exists(),
            "run_dir should be cleaned up: {}",
            run_dir.display()
        );
    }
}
