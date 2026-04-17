//! epic stage -- three tests against a programmatically generated Rust
//! project containing a deliberate bug.
//!
//! Tests run in fixed order: `leaf-task` (epic fixes the bug, oracle is
//! `cargo test`), `status` (epic status reports on the completed run),
//! `resume-completed` (epic resume on an already-finished run exits
//! cleanly without re-execution). `status` and `resume-completed`
//! materially depend on the on-disk state the leaf-task run leaves
//! behind, so a leaf-task failure short-circuits the rest of the stage
//! into `Skip` results -- running them against missing or partial state
//! would just produce derivative failures with the same root cause.
//!
//! The test project is generated, not committed: every run starts from
//! a clean state matching the current toolchain. `cargo check` is run
//! at generation time to verify the project compiles (the bug is logic,
//! not syntax); `git init/add/commit` produces a clean working tree so
//! epic can see what files it modified.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::check::{TestFailure, assert_contains, assert_exit_ok, assert_path_exists};
use crate::exec::run_command;
use crate::runner::StageContext;
use crate::types::{CommandResult, Stage, TestOutcome, TestResult};

/// Wall-clock cap for the leaf-task test specifically. Epic's
/// recursive orchestrator on a leaf bugfix usually settles within a
/// few minutes; 600s is the spec-mandated cap matching
/// `Stage::Epic.default_timeout()`. NOT used for status / resume,
/// which never make model calls.
const LEAF_TASK_TIMEOUT: Duration = Duration::from_secs(600);

/// Cargo invocations against the generated 3-file project are quick
/// (no dependencies). 180s leaves headroom for first-time toolchain
/// downloads on a cold cache without letting a hung child stall the stage.
const CARGO_TIMEOUT: Duration = Duration::from_secs(180);

/// Git plumbing is fast; the generous cap exists only to bound a hung
/// hook or AV scanner.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Wall-clock cap for state-only epic invocations: `status`, plus
/// `resume` against an already-completed run (which short-circuits
/// without re-entering the orchestrator). Both paths read only the
/// on-disk state.json and never make a model call. 60s is generous;
/// a longer cap would only delay surfacing a regression where one of
/// these paths accidentally re-enters the orchestration loop.
const STATE_ONLY_TIMEOUT: Duration = Duration::from_secs(60);

const CARGO_TOML: &str = "[package]
name = \"gate-test-project\"
version = \"0.1.0\"
edition = \"2024\"

# Empty [workspace] table prevents cargo from auto-adopting this
# project into an ancestor workspace (the gate scratch dir lives under
# the backlot workspace's target/, so cargo would otherwise refuse to
# build it without a workspace.members entry).
[workspace]
";

/// Library source containing the deliberate bug. Subtraction is used in
/// place of addition; `cargo check` succeeds (the function is well-typed)
/// but `cargo test` fails. Epic's leaf-task agent must rewrite the body
/// to perform addition.
const LIB_RS: &str = "/// Add two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    a - b // BUG: subtraction instead of addition
}
";

const TEST_RS: &str = "use gate_test_project::add;

#[test]
fn test_add() {
    assert_eq!(add(2, 3), 5);
}
";

/// Minimal `epic.toml`: tier aliases match gate's `fast`/`balanced`/
/// `strong` convention so a single set of `flick model add` registrations
/// covers every stage. The single verification step (`cargo test`) is
/// what epic's leaf agent will use to know it has fixed the bug.
const EPIC_TOML: &str = "[models]
fast = \"fast\"
balanced = \"balanced\"
strong = \"strong\"

[[verification]]
name = \"test\"
command = [\"cargo\", \"test\"]
timeout = 300
";

/// Goal handed to `epic run`. Phrased as a concrete instruction so a
/// well-prompted Sonnet-class model can complete it in one leaf task
/// without spawning a subtree -- keeping the cost of this stage bounded.
const TASK_GOAL: &str = "Fix the bug in src/lib.rs so that `cargo test` passes. The add function should return a + b, not a - b.";

pub fn run(ctx: &StageContext) -> Vec<TestResult> {
    let project_dir = match generate_test_project(&ctx.scratch_dir) {
        Ok(p) => p,
        Err(e) => {
            return vec![synthetic_setup_failure(format!(
                "epic stage setup failed: {e}"
            ))];
        }
    };

    type Step<'a> = (&'static str, Box<dyn FnOnce() -> TestResult + 'a>);
    let steps: [Step<'_>; 3] = [
        ("leaf-task", Box::new(|| test_leaf_task(ctx, &project_dir))),
        ("status", Box::new(|| test_status(ctx, &project_dir))),
        (
            "resume-completed",
            Box::new(|| test_resume_completed(ctx, &project_dir)),
        ),
    ];

    let mut results: Vec<TestResult> = Vec::with_capacity(steps.len());
    let mut failed_label: Option<String> = None;
    for (label, step) in steps {
        if let Some(ref upstream) = failed_label {
            results.push(synthetic_skip(label, upstream));
            continue;
        }
        let r = step();
        if r.outcome.is_failure() {
            failed_label = Some(label.to_string());
        }
        results.push(r);
    }
    results
}

/// Generate the test project at `<scratch_dir>/project/`. Writes the
/// Cargo manifest, source, test, and `epic.toml`; runs `cargo check` to
/// confirm the project compiles; initializes a git repo with a single
/// `initial` commit so the working tree is clean before epic starts.
///
/// Returns the project root on success. The directory is wiped first if
/// it exists, so re-invocation in the same scratch tree is idempotent.
fn generate_test_project(scratch_dir: &Path) -> io::Result<PathBuf> {
    let project = scratch_dir.join("project");
    if project.exists() {
        fs::remove_dir_all(&project)?;
    }
    fs::create_dir_all(project.join("src"))?;
    fs::create_dir_all(project.join("tests"))?;
    fs::write(project.join("Cargo.toml"), CARGO_TOML)?;
    fs::write(project.join("src").join("lib.rs"), LIB_RS)?;
    fs::write(project.join("tests").join("basic.rs"), TEST_RS)?;
    fs::write(project.join("epic.toml"), EPIC_TOML)?;

    // `cargo check` proves the bug is logic-level: the file compiles,
    // so epic's agent is exercising real reasoning rather than fixing
    // a syntax error a linter could catch.
    let check = run_command(
        "cargo",
        &["check", "--quiet"],
        Some(&project),
        &[],
        CARGO_TIMEOUT,
    )?;
    if check.exit_code != 0 {
        return Err(io::Error::other(format!(
            "cargo check failed (exit {}): {}",
            check.exit_code,
            check.stderr.trim()
        )));
    }

    git_init_and_commit(&project)?;
    Ok(project)
}

/// `git init` + initial commit. Local `user.name`/`user.email` are
/// configured before the commit because a contributor's machine may
/// have no global git identity, and `git commit` fails hard in that
/// case rather than producing a useful error chain back to gate.
fn git_init_and_commit(project: &Path) -> io::Result<()> {
    git(project, &["init", "--quiet"])?;
    git(project, &["config", "user.name", "gate"])?;
    git(project, &["config", "user.email", "gate@example.invalid"])?;
    // `commit.gpgsign=false` and `core.autocrlf=false` defang two common
    // global-config defaults that would otherwise either prompt for a
    // GPG passphrase (hangs the test stage) or rewrite line endings
    // (makes the working tree dirty after add).
    git(project, &["config", "commit.gpgsign", "false"])?;
    git(project, &["config", "core.autocrlf", "false"])?;
    git(project, &["add", "-A"])?;
    git(project, &["commit", "--quiet", "-m", "initial"])?;
    Ok(())
}

fn git(project: &Path, args: &[&str]) -> io::Result<()> {
    let out = run_command("git", args, Some(project), &[], GIT_TIMEOUT)?;
    if out.exit_code != 0 {
        return Err(io::Error::other(format!(
            "`git {}` failed (exit {}): {}",
            args.join(" "),
            out.exit_code,
            out.stderr.trim()
        )));
    }
    Ok(())
}

fn synthetic_skip(label: &str, upstream: &str) -> TestResult {
    TestResult {
        stage: Stage::Epic,
        test: label.into(),
        outcome: TestOutcome::Skip(format!("upstream test '{upstream}' failed")),
        duration: Duration::ZERO,
        cost_usd: None,
        tokens_in: None,
        tokens_out: None,
        stdout: None,
        stderr: None,
    }
}

fn synthetic_setup_failure(msg: String) -> TestResult {
    TestResult {
        stage: Stage::Epic,
        test: "gate:epic-setup".into(),
        outcome: TestOutcome::Fail(msg),
        duration: Duration::ZERO,
        cost_usd: None,
        tokens_in: None,
        tokens_out: None,
        stdout: None,
        stderr: None,
    }
}

/// Build a `TestResult` from a closure that returns `Ok(out)` (with the
/// captured CommandResult stashed for the runner's `--verbose`
/// transcript writer) or `Err(TestFailure)`. Centralizes the
/// duration-recording, transcript-stashing, and outcome-conversion
/// boilerplate so per-test functions stay focused on the assertions.
fn run_test<F>(name: &str, body: F) -> TestResult
where
    F: FnOnce() -> Result<CommandResult, TestFailure>,
{
    let start = Instant::now();
    let (outcome, stdout, stderr) = match body() {
        Ok(out) => (TestOutcome::Pass, Some(out.stdout), Some(out.stderr)),
        Err(f) => (TestOutcome::Fail(f.detail), None, None),
    };
    TestResult {
        stage: Stage::Epic,
        test: name.into(),
        outcome,
        duration: start.elapsed(),
        cost_usd: None,
        tokens_in: None,
        tokens_out: None,
        stdout,
        stderr,
    }
}

/// `leaf-task` -- the primary oracle. Run epic on the generated project,
/// then run `cargo test`. Epic's outcome is judged by whether the test
/// passes after it finishes, NOT by which tools it called or what its
/// stdout looked like (LLM behavior is non-deterministic; verification
/// gates are deterministic).
fn test_leaf_task(ctx: &StageContext, project: &Path) -> TestResult {
    let label = "leaf-task";
    run_test(label, || {
        let bin = ctx.binaries.epic.to_string_lossy().to_string();
        let epic_out = run_command(
            &bin,
            &["--no-tui", "run", TASK_GOAL],
            Some(project),
            &[],
            LEAF_TASK_TIMEOUT,
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn `epic run` failed: {e}"),
        })?;
        assert_exit_ok(&epic_out, label)?;

        // State persistence: epic must have written the run's task tree.
        // A passing epic that did not produce state.json would leave the
        // resume-completed test with nothing to operate on.
        let state_path = project.join(".epic").join("state.json");
        assert_path_exists(&state_path, &format!("{label}/state-json"))?;

        // Oracle: cargo test on the post-fix project must pass. This is
        // the authoritative signal -- if epic claimed success but the
        // bug is still present, the test fails here regardless of what
        // epic's stdout said.
        let cargo_out = run_command(
            "cargo",
            &["test", "--quiet"],
            Some(project),
            &[],
            CARGO_TIMEOUT,
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn `cargo test` failed: {e}"),
        })?;
        if cargo_out.exit_code != 0 {
            let detail = format!(
                "cargo test failed after epic run (exit {}): stdout={:?} stderr={:?}",
                cargo_out.exit_code, cargo_out.stdout, cargo_out.stderr
            );
            println!("FAIL: {label}/cargo-test: {detail}");
            return Err(TestFailure {
                label: label.into(),
                detail,
            });
        }
        println!("PASS: {label}/cargo-test (the bug is fixed)");
        Ok(epic_out)
    })
}

/// `status` -- after the leaf-task run, `epic status` must succeed and
/// report on the completed task. State persistence is verified
/// indirectly: if the state file is absent or corrupt, `epic status`
/// exits non-zero and the assertion fails.
fn test_status(ctx: &StageContext, project: &Path) -> TestResult {
    let label = "status";
    run_test(label, || {
        let bin = ctx.binaries.epic.to_string_lossy().to_string();
        let out = run_command(&bin, &["status"], Some(project), &[], STATE_ONLY_TIMEOUT).map_err(
            |e| TestFailure {
                label: label.into(),
                detail: format!("spawn `epic status` failed: {e}"),
            },
        )?;
        assert_exit_ok(&out, label)?;
        // Status output mentions the goal text on the first line; this
        // is the canonical proof that status loaded the persisted state
        // rather than reporting an empty default.
        assert_contains(&out.stdout, "Goal:", &format!("{label}/has-goal-line"))?;
        Ok(out)
    })
}

/// `resume-completed` -- `epic resume` on an already-completed run
/// must exit 0 without re-executing work. Epic's resume path detects a
/// completed root task and short-circuits; failure here usually means
/// epic re-entered the orchestration loop on completed state and
/// burned tokens.
fn test_resume_completed(ctx: &StageContext, project: &Path) -> TestResult {
    let label = "resume-completed";
    run_test(label, || {
        let bin = ctx.binaries.epic.to_string_lossy().to_string();
        let out = run_command(
            &bin,
            &["--no-tui", "resume"],
            Some(project),
            &[],
            STATE_ONLY_TIMEOUT,
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn `epic resume` failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        // After resume on a completed run epic prints `Epic completed:
        // ...` to stdout via its no-tui finishing path. The substring
        // "completed" is the contract; the variant detail (Success vs
        // SuccessWithWarnings) intentionally is not pinned.
        assert_contains(
            &out.stdout,
            "completed",
            &format!("{label}/already-complete"),
        )?;
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::scratch_base;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Project-local scratch dir for one test under
    /// `target/gate-scratch/epic-tests/`. Honors the workspace
    /// `CLAUDE.md` rule against system temp.
    struct TestDir(PathBuf);
    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = scratch_base()
                .join("epic-tests")
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

    /// Fail loudly (not silently skip) when the host environment is
    /// missing prerequisites for the project-generation tests. A
    /// silently-skipped test is a lie: it reports success when nothing
    /// was verified (workspace `CLAUDE.md` rule).
    fn require_cargo_and_git() {
        let cargo = run_command("cargo", &["--version"], None, &[], Duration::from_secs(30));
        assert!(
            cargo.is_ok() && cargo.as_ref().expect("ok").exit_code == 0,
            "cargo must be on PATH for epic stage tests; got {cargo:?}"
        );
        let git = run_command("git", &["--version"], None, &[], Duration::from_secs(30));
        assert!(
            git.is_ok() && git.as_ref().expect("ok").exit_code == 0,
            "git must be on PATH for epic stage tests; got {git:?}"
        );
    }

    /// Spec TDD test #1: the generated project compiles. Implicitly
    /// verified by `generate_test_project` itself (which runs
    /// `cargo check` and returns Err on non-zero exit), but pinned as
    /// its own test so a future refactor that drops the inline check
    /// is caught here rather than as a downstream epic failure.
    #[test]
    fn generate_test_project_compiles() {
        require_cargo_and_git();
        let td = TestDir::new("compiles");
        let project = generate_test_project(td.path()).expect("generate project");
        // Re-run cargo check explicitly so the test does not silently
        // depend on generate_test_project's internal check.
        let out = run_command(
            "cargo",
            &["check", "--quiet"],
            Some(&project),
            &[],
            CARGO_TIMEOUT,
        )
        .expect("cargo check spawn");
        assert_eq!(
            out.exit_code, 0,
            "cargo check should succeed: stderr={}",
            out.stderr
        );
    }

    /// Spec TDD test #2: the bug actually breaks the test. If this
    /// test passes (cargo test exits 0) the bug isn't real and the
    /// leaf-task oracle would be a no-op.
    #[test]
    fn generate_test_project_tests_fail() {
        require_cargo_and_git();
        let td = TestDir::new("tests-fail");
        let project = generate_test_project(td.path()).expect("generate project");
        let out = run_command(
            "cargo",
            &["test", "--quiet"],
            Some(&project),
            &[],
            CARGO_TIMEOUT,
        )
        .expect("cargo test spawn");
        assert_ne!(
            out.exit_code, 0,
            "cargo test must fail to make the bug real; stdout={}",
            out.stdout
        );
    }

    /// Spec TDD test #3: a `.git/` directory is initialized so epic
    /// can see what files it modified.
    #[test]
    fn generate_test_project_has_git() {
        require_cargo_and_git();
        let td = TestDir::new("has-git");
        let project = generate_test_project(td.path()).expect("generate project");
        let dot_git = project.join(".git");
        assert!(
            dot_git.is_dir(),
            ".git/ should exist at {}",
            dot_git.display()
        );
    }

    /// Spec TDD test #4: the working tree is clean after generation.
    /// A dirty working tree would mean epic could not distinguish its
    /// own changes from preexisting noise.
    #[test]
    fn generate_test_project_clean_working_tree() {
        require_cargo_and_git();
        let td = TestDir::new("clean-tree");
        let project = generate_test_project(td.path()).expect("generate project");
        let out = run_command(
            "git",
            &["status", "--porcelain"],
            Some(&project),
            &[],
            GIT_TIMEOUT,
        )
        .expect("git status spawn");
        assert_eq!(out.exit_code, 0, "git status: {}", out.stderr);
        assert!(
            out.stdout.trim().is_empty(),
            "working tree should be clean; got: {:?}",
            out.stdout
        );
    }
}
