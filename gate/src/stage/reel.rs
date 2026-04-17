//! reel stage -- five tests exercising reel's agent runtime CLI against a
//! real LLM provider.
//!
//! Each test seeds a per-test workspace by recursively copying
//! `gate/fixtures/reel/workspace/` into a sibling subdirectory of
//! `ctx.scratch_dir`, then invokes `reel run` with `--project-root` pointed
//! at that copy. Per-test workspaces guarantee isolation: a writing test
//! (`write-session`, `multi-turn`) cannot pollute a read-only test's view,
//! and reruns work because the source fixtures are never mutated.
//!
//! Cost is intentionally low: every test that calls a model uses the
//! cheapest tier alias (`fast`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::check::{
    TestFailure, assert_contains, assert_exit_fail, assert_exit_ok, assert_path_exists,
};
use crate::exec::run_command;
use crate::runner::StageContext;
use crate::types::{CommandResult, Stage, TestOutcome, TestResult};

/// Per-test wall-clock cap. Multi-turn agent sessions can chain several
/// tool rounds; 180s is well above the cheapest-model latency profile but
/// short enough that a hung child surfaces in a single test run, not after
/// waiting out the per-stage default timeout.
const REEL_TIMEOUT: Duration = Duration::from_secs(180);

pub fn run(ctx: &StageContext) -> Vec<TestResult> {
    vec![
        test_readonly_session(ctx),
        test_write_session(ctx),
        test_nushell_execution(ctx),
        test_multi_turn(ctx),
        test_error_invalid_model(ctx),
    ]
}

/// Resolve the path to a committed reel fixture file under
/// `gate/fixtures/reel/`. The agent configs are committed as static
/// fixtures because they reference tier aliases (`fast`) rather than
/// absolute paths -- nothing in them needs runtime substitution.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("reel")
        .join(name)
}

/// Recursively copy every file and subdirectory under `src` into `dest`,
/// creating `dest` if absent. Files that already exist at the destination
/// are overwritten. Used to seed a per-test scratch workspace from the
/// committed `gate/fixtures/reel/workspace/` tree without mutating the
/// fixture itself, so reruns and parallel tests stay isolated.
fn seed_workspace(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dest.join(entry.file_name());
        if ty.is_dir() {
            seed_workspace(&entry.path(), &dest_path)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &dest_path)?;
        }
        // Symlinks and other special entries are intentionally ignored:
        // the committed workspace fixture is plain files/dirs, and silently
        // skipping anything else avoids portability issues across OSes.
    }
    Ok(())
}

/// Spawn reel with the standard subcommand layout used by every test:
/// `reel run --config <yaml> --project-root <dir> --query <q>`.
fn run_reel(ctx: &StageContext, args: &[&str]) -> io::Result<CommandResult> {
    let bin = ctx.binaries.reel.to_string_lossy().to_string();
    run_command(&bin, args, None, &[], REEL_TIMEOUT)
}

/// Parse reel's stdout as JSON. Reel emits one line of JSON on success
/// (the `SuccessOutput` shape) and one line on failure (the `ErrorOutput`
/// shape). Both parse as a generic `Value`; per-test logic inspects the
/// `status` field to discriminate.
fn parse_result(out: &CommandResult, label: &str) -> Result<Value, TestFailure> {
    let trimmed = out.stdout.trim();
    if trimmed.is_empty() {
        return Err(TestFailure {
            label: label.to_string(),
            detail: format!(
                "reel stdout was empty (exit={}, stderr={:?})",
                out.exit_code, out.stderr
            ),
        });
    }
    serde_json::from_str(trimmed).map_err(|e| TestFailure {
        label: label.to_string(),
        detail: format!("could not parse reel stdout as JSON: {e} (raw: {trimmed:?})"),
    })
}

/// Optional usage data extracted from reel's success JSON. Threaded back
/// through `run_test` so the summary table and `results.json` carry real
/// token/cost numbers rather than zeros.
#[derive(Default)]
struct BodyOk {
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    cost_usd: Option<f64>,
}

fn extract_usage(json: &Value) -> BodyOk {
    let usage = json.get("usage").and_then(Value::as_object);
    BodyOk {
        tokens_in: usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64),
        tokens_out: usage
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64),
        cost_usd: usage
            .and_then(|u| u.get("cost_usd"))
            .and_then(Value::as_f64),
    }
}

fn run_test<F>(name: &str, body: F) -> TestResult
where
    F: FnOnce() -> Result<BodyOk, TestFailure>,
{
    let start = Instant::now();
    let (outcome, tokens_in, tokens_out, cost_usd) = match body() {
        Ok(ok) => (TestOutcome::Pass, ok.tokens_in, ok.tokens_out, ok.cost_usd),
        Err(f) => (TestOutcome::Fail(f.detail), None, None, None),
    };
    TestResult {
        stage: Stage::Reel,
        test: name.into(),
        outcome,
        duration: start.elapsed(),
        cost_usd,
        tokens_in,
        tokens_out,
    }
}

/// Assert that reel reported `status == "Ok"`. Reel's success shape uses
/// `Ok` (not `complete` like flick); a missing or different status means
/// the agent loop crashed or surfaced a CLI error.
fn assert_status_ok(json: &Value, label: &str) -> Result<(), TestFailure> {
    let got = json.get("status").and_then(Value::as_str);
    match got {
        Some("Ok") => {
            println!("PASS: {label} (status=Ok)");
            Ok(())
        }
        Some(other) => Err(TestFailure {
            label: label.into(),
            detail: format!("expected status='Ok', got '{other}'"),
        }),
        None => Err(TestFailure {
            label: label.into(),
            detail: format!("missing or non-string `status` in {json:?}"),
        }),
    }
}

/// Assert reel reported a usage block with non-zero token counts. Reel
/// always emits usage on a successful Run; a zero-token success usually
/// means the LLM call short-circuited (cached / mock) and is not a real
/// E2E exercise.
fn assert_usage_nonzero(json: &Value, label: &str) -> Result<(), TestFailure> {
    let usage = json
        .get("usage")
        .and_then(Value::as_object)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing `usage` block in {json:?}"),
        })?;
    // A response that omits a token field (or carries a non-numeric
    // value) is materially different from one that explicitly reports
    // zero: the former points at a contract regression in reel's output
    // shape, the latter at a short-circuited LLM call. Surface the two
    // cases with distinct messages.
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing or non-numeric `input_tokens` in usage block: {usage:?}"),
        })?;
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing or non-numeric `output_tokens` in usage block: {usage:?}"),
        })?;
    if input == 0 && output == 0 {
        return Err(TestFailure {
            label: label.into(),
            detail: format!("usage block has zero tokens: {usage:?}"),
        });
    }
    println!("PASS: {label}/usage (in={input}, out={output})");
    Ok(())
}

/// Flatten reel's `content` field into a single string for substring
/// assertions. Reel deserializes the agent's final response as a JSON
/// `Value`; the simplest cross-shape match is the value's serialized form.
///
/// CAVEAT FOR CALLERS: the returned text is the JSON-serialized form of
/// `content`, not the raw model output. String values are wrapped in
/// double quotes and special characters (newlines, tabs, embedded
/// quotes, non-ASCII) are escape-sequenced. Substring assertions must
/// be chosen accordingly: punctuation-heavy or quote-bearing literals
/// will silently mismatch against the serialized form. Stick to plain
/// ASCII fragments that survive JSON escaping unchanged
/// (e.g. "Hello, World!" matches because `,` and `!` are not escaped).
fn content_as_text(json: &Value) -> String {
    match json.get("content") {
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

// -- Tests --------------------------------------------------------------

fn test_readonly_session(ctx: &StageContext) -> TestResult {
    let label = "readonly-session";
    run_test(label, || {
        let workspace = ctx.scratch_dir.join("readonly-workspace");
        seed_workspace(&fixture_path("workspace"), &workspace).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("seed workspace: {e}"),
        })?;
        let cfg = fixture_path("readonly.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let workspace_str = workspace.to_string_lossy().to_string();
        let out = run_reel(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--project-root",
                &workspace_str,
                "--query",
                "List the files in the project root, then read hello.txt and report its exact contents in your reply.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_ok(&json, label)?;
        assert_contains(
            &content_as_text(&json),
            "Hello, World!",
            &format!("{label}/content"),
        )?;
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_write_session(ctx: &StageContext) -> TestResult {
    let label = "write-session";
    run_test(label, || {
        let workspace = ctx.scratch_dir.join("write-workspace");
        seed_workspace(&fixture_path("workspace"), &workspace).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("seed workspace: {e}"),
        })?;
        let cfg = fixture_path("write.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let workspace_str = workspace.to_string_lossy().to_string();
        let out = run_reel(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--project-root",
                &workspace_str,
                "--query",
                "Create a file named output.txt in the project root containing exactly the text: test output",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_ok(&json, label)?;
        let written = workspace.join("output.txt");
        assert_path_exists(&written, &format!("{label}/file-created"))?;
        // Content sanity-check: read the file and verify "test output" is
        // present. We do not require an exact byte match because the model
        // may include a trailing newline or normalize whitespace.
        let body = fs::read_to_string(&written).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("read written file {}: {e}", written.display()),
        })?;
        assert_contains(&body, "test output", &format!("{label}/file-content"))?;
        Ok(extract_usage(&json))
    })
}

fn test_nushell_execution(ctx: &StageContext) -> TestResult {
    let label = "nushell-execution";
    run_test(label, || {
        let workspace = ctx.scratch_dir.join("nushell-workspace");
        seed_workspace(&fixture_path("workspace"), &workspace).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("seed workspace: {e}"),
        })?;
        let cfg = fixture_path("nushell.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let workspace_str = workspace.to_string_lossy().to_string();
        let out = run_reel(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--project-root",
                &workspace_str,
                "--query",
                "Use the NuShell tool to compute 2 + 2 and report the result.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_ok(&json, label)?;
        // Whether the model picks NuShell vs another path is
        // non-deterministic; the spec only requires the session to
        // complete without crash. A non-zero usage block confirms the
        // model was actually invoked.
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_multi_turn(ctx: &StageContext) -> TestResult {
    let label = "multi-turn";
    run_test(label, || {
        let workspace = ctx.scratch_dir.join("multiturn-workspace");
        seed_workspace(&fixture_path("workspace"), &workspace).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("seed workspace: {e}"),
        })?;
        let cfg = fixture_path("write.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let workspace_str = workspace.to_string_lossy().to_string();
        let out = run_reel(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--project-root",
                &workspace_str,
                "--query",
                "Read hello.txt, then read data.json, then write a file summary.txt that contains the exact text from hello.txt followed by the exact text from data.json.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_ok(&json, label)?;
        let summary = workspace.join("summary.txt");
        assert_path_exists(&summary, &format!("{label}/summary-created"))?;
        // Reel's SuccessOutput exposes `tool_calls`; the multi-step task
        // must trigger more than one tool invocation (read + read + write
        // at minimum). Anything below 2 either means the model skipped a
        // step or the count plumbing regressed.
        let calls = json
            .get("tool_calls")
            .and_then(Value::as_u64)
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: format!("missing or non-numeric `tool_calls` in {json:?}"),
            })?;
        if calls < 2 {
            return Err(TestFailure {
                label: label.into(),
                detail: format!("expected tool_calls > 1, got {calls}"),
            });
        }
        println!("PASS: {label}/tool-calls (count={calls})");
        Ok(extract_usage(&json))
    })
}

fn test_error_invalid_model(ctx: &StageContext) -> TestResult {
    let label = "error-invalid-model";
    run_test(label, || {
        // Intentionally skip seed_workspace: model alias resolution
        // fails before reel touches any tool or file. Seeding a workspace
        // would just be wasted I/O on the hot path. Pass the empty
        // scratch dir as --project-root so reel still gets a real
        // existing path on disk.
        let cfg = fixture_path("invalid_model.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let workspace_str = ctx.scratch_dir.to_string_lossy();
        let out = run_reel(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--project-root",
                workspace_str.as_ref(),
                "--query",
                "ignored -- model alias resolution fails before any API call",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_fail(&out, label)?;
        Ok(BodyOk::default())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::scratch_base;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Project-local scratch dir for one test under
    /// `target/gate-scratch/reel-tests/`. Honors the workspace `CLAUDE.md`
    /// rule against system temp for sandbox-relevant paths.
    struct TestDir(PathBuf);
    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = scratch_base()
                .join("reel-tests")
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

    /// Spec TDD test: copying the committed workspace fixture into a
    /// scratch destination produces the expected file structure with
    /// byte-identical contents.
    #[test]
    fn reel_workspace_seeding() {
        let td = TestDir::new("seed");
        let dest = td.path().join("workspace-copy");
        seed_workspace(&fixture_path("workspace"), &dest).expect("seed workspace");

        let hello = dest.join("hello.txt");
        let data = dest.join("data.json");
        assert!(hello.is_file(), "hello.txt missing at {}", hello.display());
        assert!(data.is_file(), "data.json missing at {}", data.display());

        let hello_body = fs::read_to_string(&hello).expect("read hello.txt");
        assert!(
            hello_body.contains("Hello, World!"),
            "hello.txt does not contain expected text, got {hello_body:?}"
        );

        let data_body = fs::read_to_string(&data).expect("read data.json");
        let parsed: serde_json::Value =
            serde_json::from_str(data_body.trim()).expect("data.json must parse as JSON");
        assert_eq!(parsed["key"], "value");
        assert_eq!(parsed["items"], serde_json::json!([1, 2, 3]));
    }

    /// Spec TDD test: every fixture file the stage relies on is present
    /// on disk under `gate/fixtures/reel/`. Catches typos or missing
    /// commits before the integration tests waste tokens spinning up
    /// reel only to discover a missing config.
    #[test]
    fn reel_fixture_files_exist() {
        for name in [
            "readonly.yaml",
            "write.yaml",
            "nushell.yaml",
            "invalid_model.yaml",
        ] {
            let p = fixture_path(name);
            assert!(p.is_file(), "fixture missing: {}", p.display());
        }
        let workspace = fixture_path("workspace");
        assert!(
            workspace.is_dir(),
            "workspace fixture dir missing: {}",
            workspace.display()
        );
        for f in ["hello.txt", "data.json"] {
            let p = workspace.join(f);
            assert!(p.is_file(), "workspace seed missing: {}", p.display());
        }
    }

    #[test]
    fn seed_workspace_is_idempotent() {
        // The stage runs each test in its own subdir; rerunning the
        // copy on a populated destination must not error and must leave
        // file contents intact.
        let td = TestDir::new("idem");
        let dest = td.path().join("ws");
        seed_workspace(&fixture_path("workspace"), &dest).expect("seed 1");
        seed_workspace(&fixture_path("workspace"), &dest).expect("seed 2");
        assert!(dest.join("hello.txt").is_file());
    }

    #[test]
    fn parse_result_rejects_empty_stdout() {
        let cr = CommandResult {
            stdout: String::new(),
            stderr: "boom".into(),
            exit_code: 1,
            duration: Duration::ZERO,
        };
        let err = parse_result(&cr, "x").expect_err("empty stdout");
        assert!(err.detail.contains("empty"), "{}", err.detail);
    }

    #[test]
    fn parse_result_accepts_ok_result() {
        let cr = CommandResult {
            stdout: r#"{"status":"Ok","content":"hi","tool_calls":1,"usage":{"input_tokens":3,"output_tokens":1,"cost_usd":0.0}}"#.into(),
            stderr: String::new(),
            exit_code: 0,
            duration: Duration::ZERO,
        };
        let v = parse_result(&cr, "x").expect("parse");
        assert_eq!(v["status"], "Ok");
    }

    #[test]
    fn extract_usage_pulls_token_fields() {
        let v: Value = serde_json::from_str(
            r#"{"usage":{"input_tokens":12,"output_tokens":34,"cost_usd":0.005}}"#,
        )
        .unwrap();
        let u = extract_usage(&v);
        assert_eq!(u.tokens_in, Some(12));
        assert_eq!(u.tokens_out, Some(34));
        assert_eq!(u.cost_usd, Some(0.005));
    }

    #[test]
    fn assert_status_ok_accepts_ok_and_rejects_others() {
        let ok: Value = serde_json::from_str(r#"{"status":"Ok"}"#).unwrap();
        assert_status_ok(&ok, "x").expect("Ok status accepted");
        let bad: Value = serde_json::from_str(r#"{"status":"Error"}"#).unwrap();
        let err = assert_status_ok(&bad, "x").expect_err("non-Ok rejected");
        assert!(err.detail.contains("Ok"), "{}", err.detail);
    }

    #[test]
    fn assert_usage_nonzero_rejects_zero_block() {
        let v: Value =
            serde_json::from_str(r#"{"usage":{"input_tokens":0,"output_tokens":0}}"#).unwrap();
        let err = assert_usage_nonzero(&v, "x").expect_err("zero tokens rejected");
        assert!(err.detail.contains("zero tokens"), "{}", err.detail);
    }

    #[test]
    fn content_as_text_handles_missing_field() {
        let v: Value = serde_json::from_str(r#"{"status":"Ok"}"#).unwrap();
        assert!(content_as_text(&v).is_empty());
    }

    #[test]
    fn content_as_text_serializes_value() {
        let v: Value = serde_json::from_str(r#"{"content":"Hello, World!"}"#).unwrap();
        // String values serialize with surrounding quotes; the substring
        // assertion in the live test handles this transparently because
        // the literal `Hello, World!` is preserved either way.
        assert!(content_as_text(&v).contains("Hello, World!"));
    }

    #[test]
    fn assert_usage_nonzero_rejects_missing_input_tokens() {
        // Omits input_tokens entirely; must surface the distinct
        // "missing or non-numeric" message rather than silently treating
        // the field as zero.
        let v: Value = serde_json::from_str(r#"{"usage":{"output_tokens":7}}"#).unwrap();
        let err = assert_usage_nonzero(&v, "x").expect_err("missing input_tokens rejected");
        assert!(
            err.detail.contains("missing or non-numeric `input_tokens`"),
            "{}",
            err.detail
        );
    }

    #[test]
    fn assert_usage_nonzero_rejects_non_numeric_input_tokens() {
        let v: Value =
            serde_json::from_str(r#"{"usage":{"input_tokens":"oops","output_tokens":7}}"#).unwrap();
        let err = assert_usage_nonzero(&v, "x").expect_err("non-numeric input_tokens rejected");
        assert!(
            err.detail.contains("missing or non-numeric `input_tokens`"),
            "{}",
            err.detail
        );
    }
}
