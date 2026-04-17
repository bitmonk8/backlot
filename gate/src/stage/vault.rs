//! vault stage -- five tests exercising vault's knowledge-store CLI.
//!
//! Vault tests are SEQUENTIAL and SHARED-STATE: each later test depends
//! on the artifacts produced by an earlier one (bootstrap creates the
//! store, record adds to it, query reads from it, reorganize rewrites
//! it). The execution order is fixed by [`run`]; do not parallelize.
//!
//! Stage setup wipes and recreates the on-disk store directory under
//! `ctx.scratch_dir/store/` exactly once before the bootstrap test runs,
//! and writes a per-run `runtime-config.yaml` whose `storage_root` points
//! at that directory. The committed `gate/fixtures/vault/config.yaml`
//! cannot be used directly because vault's `storage_root` must be an
//! absolute, per-run path; the committed file is a stub for documentation
//! and for the `vault_config_fixture_exists` unit test.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::check::{TestFailure, assert_contains, assert_exit_ok, assert_path_exists};
use crate::exec::{run_command, run_command_with_stdin};
use crate::runner::StageContext;
use crate::types::{CommandResult, Stage, TestOutcome, TestResult};

/// Per-test wall-clock cap. Vault's bootstrap and reorganize sessions are
/// the heaviest librarian calls in the suite; 300s leaves headroom on a
/// slow network without letting a hung child stall the stage.
const VAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Requirements text piped into `vault bootstrap`. Small but meaningful
/// enough that the bootstrap pass produces real raw + derived artifacts
/// against any reasonable librarian model. Embedded here (not a fixture
/// file) because the text is short and the test code is the only consumer.
const BOOTSTRAP_REQUIREMENTS: &str = "\
Project: backlot gate stage 4 vault smoke test.
Goal: exercise vault's knowledge store end to end.
Components: a small documentation set with two short topics.
Topic A: greeting protocol -- documents must record the literal phrase \"Hello, World!\" \
as the canonical greeting used by the system.
Topic B: data shape -- the project's primary data record has a single key named \"key\" \
with the string value \"value\", and an items array containing the integers 1, 2, 3.";

/// Document name used by the record-new and record-append tests. UPPERCASE
/// because vault's CLI requires uppercase series names.
const RECORD_DOC_NAME: &str = "GREETING";

/// Initial content piped into `vault record --mode new`.
const RECORD_NEW_CONTENT: &str =
    "The canonical greeting used by the system is the literal phrase \"Hello, World!\".";

/// Additional content piped into `vault record --mode append`.
const RECORD_APPEND_CONTENT: &str =
    "Operators must reproduce the greeting exactly, including punctuation and capitalization.";

/// Question piped into `vault query`. References content recorded by the
/// preceding tests so a passing query response is an end-to-end signal,
/// not just a model artifact.
const QUERY_QUESTION: &str = "What is the canonical greeting used by the system?";

pub fn run(ctx: &StageContext) -> Vec<TestResult> {
    let store_dir = ctx.scratch_dir.join("store");
    let config_path = ctx.scratch_dir.join("runtime-config.yaml");

    // One-time stage setup: wipe the store dir and write a per-run
    // runtime config pointing at it. A failure here yields a single
    // synthetic Fail result rather than running the rest of the stage
    // against an inconsistent environment.
    if let Err(e) = setup_stage(&store_dir, &config_path) {
        return vec![synthetic_setup_failure(format!(
            "vault stage setup failed: {e}"
        ))];
    }

    // Tests are run in declared order. Because each test materially
    // depends on the on-disk state produced by the previous one, we
    // short-circuit after the first hard Fail and report the remaining
    // tests as Skip -- running them against a known-broken store would
    // just produce derivative failures with the same root cause.
    type Step<'a> = (&'static str, Box<dyn FnOnce() -> TestResult + 'a>);
    let steps: [Step<'_>; 5] = [
        (
            "bootstrap",
            Box::new(|| test_bootstrap(ctx, &config_path, &store_dir)),
        ),
        (
            "record-new",
            Box::new(|| test_record_new(ctx, &config_path, &store_dir)),
        ),
        (
            "record-append",
            Box::new(|| test_record_append(ctx, &config_path, &store_dir)),
        ),
        ("query", Box::new(|| test_query(ctx, &config_path))),
        (
            "reorganize",
            Box::new(|| test_reorganize(ctx, &config_path, &store_dir)),
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

/// Build a `Skip` result for a test that was bypassed because an earlier
/// vault test in the same run failed. The on-disk state is no longer in
/// a known-good shape, so executing this test would only produce a
/// derivative failure with the same root cause as the upstream test.
fn synthetic_skip(label: &str, upstream: &str) -> TestResult {
    TestResult {
        stage: Stage::Vault,
        test: label.into(),
        outcome: TestOutcome::Skip(format!("upstream test '{upstream}' failed")),
        duration: Duration::ZERO,
        cost_usd: None,
        tokens_in: None,
        tokens_out: None,
    }
}

/// Resolve the path to a committed vault fixture file under
/// `gate/fixtures/vault/`. The committed `config.yaml` is a stub used by
/// the `vault_config_fixture_exists` unit test; the runtime config the
/// stage actually invokes vault with is generated at setup time.
#[cfg(test)]
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("vault")
        .join(name)
}

/// Wipe and recreate `store_dir`, then write `config_path` with a vault
/// config whose `storage_root` is the absolute path of the freshly-created
/// store. The single setup pass guarantees every per-test invocation sees
/// the same store layout the bootstrap test produced.
fn setup_stage(store_dir: &Path, config_path: &Path) -> io::Result<()> {
    cleanup_vault_store(store_dir)?;
    fs::create_dir_all(store_dir)?;
    let yaml = render_runtime_config(store_dir);
    fs::write(config_path, yaml)?;
    Ok(())
}

/// Wipe every file under `store_dir`. No-op if the directory does not
/// exist. After this call the directory itself is absent; the caller is
/// expected to recreate it before vault's first invocation. Surfaced as a
/// standalone helper so the unit test can assert the post-condition.
fn cleanup_vault_store(store_dir: &Path) -> io::Result<()> {
    if store_dir.exists() {
        fs::remove_dir_all(store_dir)?;
    }
    Ok(())
}

/// Render the runtime YAML config string for a given store path. Tier
/// aliases match the `fast`/`balanced`/`strong` convention used by the
/// rest of gate's fixtures so a single set of model-alias registrations
/// covers every stage.
///
/// SAFETY / INPUT ASSUMPTIONS: this helper hand-rolls YAML by string
/// interpolation. It does NOT escape embedded `"` or newline characters
/// in `store_dir`. It is safe ONLY for gate-controlled scratch paths
/// rooted under `target/gate-scratch/`, which never contain quotes or
/// newlines. Passing arbitrary user input would produce malformed YAML
/// or, worse, YAML injection. The `debug_assert!` below is a tripwire
/// for the assumption; if it ever fires in test builds, switch to a
/// proper YAML serializer before promoting the change.
fn render_runtime_config(store_dir: &Path) -> String {
    // Forward slashes in the path so the YAML parser does not interpret
    // backslash escapes on Windows. PathBuf::display preserves the OS
    // separator; we normalize explicitly.
    let normalized = store_dir.to_string_lossy().replace('\\', "/");
    debug_assert!(
        !normalized.contains('"') && !normalized.contains('\n'),
        "render_runtime_config requires a quote/newline-free path; got {normalized:?}"
    );
    format!(
        "# Generated at runtime by gate's vault stage; do not commit.\n\
         storage_root: \"{normalized}\"\n\
         models:\n  \
           bootstrap: \"balanced\"\n  \
           query: \"fast\"\n  \
           record: \"fast\"\n  \
           reorganize: \"balanced\"\n"
    )
}

/// Build a synthetic `Fail` result reported when stage setup itself
/// failed. The single result short-circuits the rest of the stage so the
/// summary table makes the cause obvious, rather than running 5 tests
/// that all fail with the same root cause.
fn synthetic_setup_failure(msg: String) -> TestResult {
    TestResult {
        stage: Stage::Vault,
        test: "gate:vault-setup".into(),
        outcome: TestOutcome::Fail(msg),
        duration: Duration::ZERO,
        cost_usd: None,
        tokens_in: None,
        tokens_out: None,
    }
}

/// Spawn vault with the given args.
fn run_vault(ctx: &StageContext, args: &[&str]) -> io::Result<CommandResult> {
    let bin = ctx.binaries.vault.to_string_lossy().to_string();
    run_command(&bin, args, None, &[], VAULT_TIMEOUT)
}

/// Spawn vault with `stdin_bytes` piped to its standard input. Used for
/// `bootstrap` and `record` (both consume their primary payload from
/// stdin).
fn run_vault_with_stdin(
    ctx: &StageContext,
    args: &[&str],
    stdin_bytes: &[u8],
) -> io::Result<CommandResult> {
    let bin = ctx.binaries.vault.to_string_lossy().to_string();
    run_command_with_stdin(&bin, args, None, &[], VAULT_TIMEOUT, stdin_bytes)
}

/// Parse vault's stdout as JSON. Vault prints pretty-printed JSON
/// (multi-line); both record and query commands emit a single JSON
/// document followed by a trailing newline.
fn parse_result(out: &CommandResult, label: &str) -> Result<Value, TestFailure> {
    let trimmed = out.stdout.trim();
    if trimmed.is_empty() {
        return Err(TestFailure {
            label: label.to_string(),
            detail: format!(
                "vault stdout was empty (exit={}, stderr={:?})",
                out.exit_code, out.stderr
            ),
        });
    }
    serde_json::from_str(trimmed).map_err(|e| TestFailure {
        label: label.to_string(),
        detail: format!("could not parse vault stdout as JSON: {e} (raw: {trimmed:?})"),
    })
}

/// Optional usage data extracted from vault's JSON output. Vault always
/// nests usage under a top-level `usage` block with `input_tokens`,
/// `output_tokens`, `tool_calls`, and (when non-zero) `cost_usd`.
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
        stage: Stage::Vault,
        test: name.into(),
        outcome,
        duration: start.elapsed(),
        cost_usd,
        tokens_in,
        tokens_out,
    }
}

/// Assert vault's JSON output carries a usage block with non-zero token
/// counts. Vault always emits usage on a successful command; a zero block
/// usually means the librarian short-circuited (cached / mock) and the
/// E2E exercise did not actually happen.
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
    // zero: the former points at a contract regression in vault's
    // output shape, the latter at a short-circuited model invocation.
    // Surface the two cases with distinct messages.
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

/// Walk `dir` recursively and return the path to the regular file whose
/// lowercased file name contains `needle` and whose full path string is
/// lexicographically greatest among all matches. Used to locate vault's
/// changelog and per-document raw files without hard-coding vault's
/// internal naming convention (which may evolve).
///
/// `fs::read_dir` yields entries in filesystem-defined order, which is
/// not stable across platforms or runs. If vault rotates an artifact
/// (e.g. produces both `changelog.md` and `changelog-20260417.md`), a
/// "first match wins" strategy could nondeterministically target either
/// file across invocations of the same test. Sorting by full path string
/// and returning the greatest gives a deterministic choice and also
/// preferentially picks timestamp-suffixed (most-recently-named) files
/// when vault embeds dates in its filenames.
fn find_file_containing(dir: &Path, needle: &str) -> io::Result<Option<PathBuf>> {
    let mut hits: Vec<PathBuf> = Vec::new();
    collect_files_containing(dir, needle, &mut hits)?;
    hits.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    Ok(hits.pop())
}

/// Recursive helper for [`find_file_containing`]. Pushes every matching
/// file into `out`; ordering is deferred to the caller so the
/// determinism guarantee lives in one place.
fn collect_files_containing(dir: &Path, needle: &str, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() {
            collect_files_containing(&path, needle, out)?;
        } else if ty.is_file() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if name.contains(needle) {
                out.push(path);
            }
        }
    }
    Ok(())
}

/// Count the lines in a file. Used by the reorganize test to compare
/// changelog growth between bootstrap and reorganize.
fn count_lines(path: &Path) -> io::Result<usize> {
    let body = fs::read_to_string(path)?;
    Ok(body.lines().count())
}

// -- Tests --------------------------------------------------------------
//
// Tests must run in declared order; later tests depend on earlier
// state. The order is enforced by `run` invoking them sequentially.

fn test_bootstrap(ctx: &StageContext, config_path: &Path, store_dir: &Path) -> TestResult {
    let label = "bootstrap";
    run_test(label, || {
        let cfg = config_path.to_string_lossy().to_string();
        let out = run_vault_with_stdin(
            ctx,
            &["bootstrap", "--config", &cfg],
            BOOTSTRAP_REQUIREMENTS.as_bytes(),
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_path_exists(&store_dir.join("raw"), &format!("{label}/raw-dir"))?;
        assert_path_exists(&store_dir.join("derived"), &format!("{label}/derived-dir"))?;
        // Vault writes a changelog under the store; locate it by name
        // contains "changelog" so the test does not depend on the exact
        // committed filename ("changelog.md" today, possibly something
        // else later).
        let changelog = find_file_containing(store_dir, "changelog")
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("scan store for changelog: {e}"),
            })?
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: format!("no changelog file found under {}", store_dir.display()),
            })?;
        println!("PASS: {label}/changelog ({})", changelog.display());
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_record_new(ctx: &StageContext, config_path: &Path, store_dir: &Path) -> TestResult {
    let label = "record-new";
    run_test(label, || {
        let cfg = config_path.to_string_lossy().to_string();
        let out = run_vault_with_stdin(
            ctx,
            &[
                "record",
                "--config",
                &cfg,
                "--name",
                RECORD_DOC_NAME,
                "--mode",
                "new",
            ],
            RECORD_NEW_CONTENT.as_bytes(),
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        // Vault stores raw documents under <store>/raw/. The exact
        // filename includes the series name; locate by lowercased
        // substring so a future renaming convention does not break us.
        let needle = RECORD_DOC_NAME.to_ascii_lowercase();
        let doc = find_file_containing(&store_dir.join("raw"), &needle)
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("scan raw/ for new doc: {e}"),
            })?
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: format!(
                    "no raw document containing '{needle}' was created under {}",
                    store_dir.display()
                ),
            })?;
        println!("PASS: {label}/doc-created ({})", doc.display());
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_record_append(ctx: &StageContext, config_path: &Path, store_dir: &Path) -> TestResult {
    let label = "record-append";
    run_test(label, || {
        let cfg = config_path.to_string_lossy().to_string();
        // Capture pre-state so the assertion can prove the file actually
        // grew rather than being silently rewritten in place at the same
        // size.
        let needle = RECORD_DOC_NAME.to_ascii_lowercase();
        let raw_dir = store_dir.join("raw");
        let pre_doc = find_file_containing(&raw_dir, &needle)
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("scan raw/ for pre-state: {e}"),
            })?
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: "record-new must have created a document for record-append to extend"
                    .into(),
            })?;
        let pre_size = fs::metadata(&pre_doc)
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("metadata for {}: {e}", pre_doc.display()),
            })?
            .len();

        let out = run_vault_with_stdin(
            ctx,
            &[
                "record",
                "--config",
                &cfg,
                "--name",
                RECORD_DOC_NAME,
                "--mode",
                "append",
            ],
            RECORD_APPEND_CONTENT.as_bytes(),
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;

        // The post-append document may live at the same path or in a
        // sibling file (vault may version per record); use the latest
        // matching file.
        let post_doc = find_file_containing(&raw_dir, &needle)
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("scan raw/ for post-state: {e}"),
            })?
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: format!(
                    "no raw document containing '{needle}' present after append; pre was {}",
                    pre_doc.display()
                ),
            })?;
        let post_body = fs::read_to_string(&post_doc).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("read post {}: {e}", post_doc.display()),
        })?;
        let post_size = post_body.len() as u64;
        // The marker substring is the canonical proof that the appended
        // payload reached the on-disk document. Size growth alone is not
        // sufficient -- a vault bug could grow the file with arbitrary
        // garbage and still pass a size-only check -- so REQUIRE the
        // marker. The grew flag is retained for diagnostic logging only.
        let grew = post_size > pre_size;
        let contains_marker = post_body.contains("Operators must reproduce");
        if !contains_marker {
            return Err(TestFailure {
                label: label.into(),
                detail: format!(
                    "record-append did not write the expected marker text \
                     'Operators must reproduce' into {} \
                     (pre={pre_size}, post={post_size}, grew={grew})",
                    post_doc.display()
                ),
            });
        }
        println!("PASS: {label}/doc-marker (pre={pre_size}, post={post_size}, grew={grew})");
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_query(ctx: &StageContext, config_path: &Path) -> TestResult {
    let label = "query";
    run_test(label, || {
        let cfg = config_path.to_string_lossy().to_string();
        let out = run_vault(ctx, &["query", "--config", &cfg, "--query", QUERY_QUESTION]).map_err(
            |e| TestFailure {
                label: label.into(),
                detail: format!("spawn failed: {e}"),
            },
        )?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        // The whole JSON document is the assertion surface: the answer
        // must reference content from the recorded documents. Stringify
        // and check for the canonical greeting marker.
        let body = serde_json::to_string(&json).unwrap_or_default();
        if body.is_empty() {
            return Err(TestFailure {
                label: label.into(),
                detail: "vault query produced an empty JSON body".into(),
            });
        }
        assert_contains(
            &body,
            "Hello, World!",
            &format!("{label}/answer-references"),
        )?;
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_reorganize(ctx: &StageContext, config_path: &Path, store_dir: &Path) -> TestResult {
    let label = "reorganize";
    run_test(label, || {
        let cfg = config_path.to_string_lossy().to_string();
        let changelog = find_file_containing(store_dir, "changelog")
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("scan for pre-reorganize changelog: {e}"),
            })?
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: "no changelog present pre-reorganize (bootstrap must run first)".into(),
            })?;
        let pre_lines = count_lines(&changelog).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("count_lines pre {}: {e}", changelog.display()),
        })?;

        let out = run_vault(ctx, &["reorganize", "--config", &cfg]).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;

        // Re-locate the changelog rather than reusing the pre path:
        // reorganize is allowed to rotate or replace it.
        let post_changelog = find_file_containing(store_dir, "changelog")
            .map_err(|e| TestFailure {
                label: label.into(),
                detail: format!("scan for post-reorganize changelog: {e}"),
            })?
            .ok_or_else(|| TestFailure {
                label: label.into(),
                detail: "no changelog present post-reorganize".into(),
            })?;
        let post_lines = count_lines(&post_changelog).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("count_lines post {}: {e}", post_changelog.display()),
        })?;
        if post_lines <= pre_lines {
            return Err(TestFailure {
                label: label.into(),
                detail: format!(
                    "changelog did not grow during reorganize (pre={pre_lines}, post={post_lines})"
                ),
            });
        }
        println!("PASS: {label}/changelog-grew (pre={pre_lines}, post={post_lines})");
        assert_usage_nonzero(&json, label)?;
        Ok(extract_usage(&json))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::scratch_base;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Project-local scratch dir for one test under
    /// `target/gate-scratch/vault-tests/`. Honors the workspace
    /// `CLAUDE.md` rule against system temp.
    struct TestDir(PathBuf);
    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = scratch_base()
                .join("vault-tests")
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

    /// Spec TDD test: the committed stub `gate/fixtures/vault/config.yaml`
    /// exists. Even though the runtime config is generated per-run, a
    /// stub fixture documents the schema and lets a future contributor
    /// see the expected shape without grepping the source.
    #[test]
    fn vault_config_fixture_exists() {
        let p = fixture_path("config.yaml");
        assert!(
            p.is_file(),
            "vault config stub fixture missing at {}",
            p.display()
        );
    }

    /// Spec TDD test: `cleanup_vault_store` makes the directory absent
    /// (so a subsequent `create_dir_all` can rebuild it from scratch).
    /// Anything left behind would leak state across stage runs.
    #[test]
    fn vault_store_cleanup() {
        let td = TestDir::new("cleanup");
        let store = td.path().join("store");
        // Populate with content to prove the cleanup is recursive.
        fs::create_dir_all(store.join("raw")).expect("mkdirs");
        fs::write(store.join("raw").join("doc.md"), b"x").expect("seed file");
        fs::create_dir_all(store.join("derived")).expect("mkdirs");
        fs::write(store.join("changelog.md"), b"start\n").expect("changelog");
        assert!(store.exists());

        cleanup_vault_store(&store).expect("cleanup");

        // The contract is "directory absent" -- the caller recreates it.
        assert!(
            !store.exists(),
            "store dir should be gone after cleanup, found {}",
            store.display()
        );
    }

    #[test]
    fn cleanup_vault_store_is_ok_when_absent() {
        let td = TestDir::new("absent");
        let store = td.path().join("never-created");
        cleanup_vault_store(&store).expect("cleanup of missing dir is a no-op");
    }

    #[test]
    fn render_runtime_config_uses_forward_slashes() {
        // Round-trip the rendered YAML through serde_yml-equivalent
        // string assertions: storage_root must be a single quoted string,
        // backslashes must be normalized so the YAML parser does not
        // interpret them as escape sequences.
        let p = PathBuf::from(r"C:\some\windows\path");
        let yaml = render_runtime_config(&p);
        assert!(
            yaml.contains("storage_root: \"C:/some/windows/path\""),
            "expected forward slashes, got: {yaml}"
        );
        assert!(
            yaml.contains("models:"),
            "expected models section, got: {yaml}"
        );
        for tier in ["bootstrap:", "query:", "record:", "reorganize:"] {
            assert!(yaml.contains(tier), "expected '{tier}' in {yaml}");
        }
    }

    #[test]
    fn render_runtime_config_round_trips_unix_paths() {
        let p = PathBuf::from("/var/lib/vault/store");
        let yaml = render_runtime_config(&p);
        assert!(
            yaml.contains("storage_root: \"/var/lib/vault/store\""),
            "got: {yaml}"
        );
    }

    #[test]
    fn setup_stage_writes_config_and_clean_store() {
        let td = TestDir::new("setup");
        let store = td.path().join("store");
        let cfg = td.path().join("rt.yaml");
        // Pre-populate the store so we can prove setup wipes it.
        fs::create_dir_all(store.join("raw")).expect("mkdirs");
        fs::write(store.join("raw").join("stale.md"), b"stale").expect("stale file");

        setup_stage(&store, &cfg).expect("setup");

        // Store dir exists and is empty.
        assert!(store.is_dir(), "store dir not present");
        let entries: Vec<_> = fs::read_dir(&store).expect("readdir").collect();
        assert!(entries.is_empty(), "store should be empty: {entries:?}");

        // Config file written and references the store dir.
        let body = fs::read_to_string(&cfg).expect("read cfg");
        let want = store.to_string_lossy().replace('\\', "/");
        assert!(
            body.contains(&want),
            "config did not embed store path '{want}': {body}"
        );
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
    fn parse_result_accepts_object() {
        let cr = CommandResult {
            stdout: "{\"warnings\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}".into(),
            stderr: String::new(),
            exit_code: 0,
            duration: Duration::ZERO,
        };
        let v = parse_result(&cr, "x").expect("parse");
        assert!(v.is_object());
    }

    #[test]
    fn extract_usage_pulls_token_fields() {
        let v: Value = serde_json::from_str(
            "{\"usage\":{\"input_tokens\":12,\"output_tokens\":34,\"cost_usd\":0.005}}",
        )
        .unwrap();
        let u = extract_usage(&v);
        assert_eq!(u.tokens_in, Some(12));
        assert_eq!(u.tokens_out, Some(34));
        assert_eq!(u.cost_usd, Some(0.005));
    }

    #[test]
    fn assert_usage_nonzero_rejects_zero_block() {
        let v: Value =
            serde_json::from_str("{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}").unwrap();
        let err = assert_usage_nonzero(&v, "x").expect_err("zero tokens rejected");
        assert!(err.detail.contains("zero tokens"), "{}", err.detail);
    }

    #[test]
    fn find_file_containing_walks_recursively() {
        let td = TestDir::new("find");
        let nested = td.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).expect("mkdirs");
        fs::write(nested.join("CHANGELOG.md"), b"x").expect("seed");
        let found = find_file_containing(td.path(), "changelog")
            .expect("walk")
            .expect("hit");
        assert!(found.ends_with("CHANGELOG.md"), "got {}", found.display());
    }

    #[test]
    fn find_file_containing_returns_none_when_absent() {
        let td = TestDir::new("find-miss");
        let r = find_file_containing(td.path(), "nope").expect("walk");
        assert!(r.is_none());
    }

    #[test]
    fn count_lines_counts_terminated_lines() {
        let td = TestDir::new("count");
        let p = td.path().join("f.txt");
        fs::write(&p, b"a\nb\nc\n").expect("seed");
        assert_eq!(count_lines(&p).expect("count"), 3);
    }

    #[test]
    fn synthetic_skip_carries_upstream_label() {
        // Direct unit testing of `run` requires real binaries; instead
        // pin the helper that materializes the per-skip TestResult so a
        // refactor cannot silently drop the upstream attribution.
        let r = synthetic_skip("reorganize", "bootstrap");
        assert_eq!(r.test, "reorganize");
        assert_eq!(r.stage, Stage::Vault);
        match &r.outcome {
            TestOutcome::Skip(reason) => {
                assert!(
                    reason.contains("bootstrap"),
                    "skip reason should reference upstream label, got {reason:?}"
                );
            }
            other => panic!("expected Skip outcome, got {other:?}"),
        }
    }

    #[test]
    fn find_file_containing_returns_lex_greatest_when_multiple_match() {
        // Two files in the same directory, both matching the needle.
        // The lex-greatest path string must win regardless of the order
        // `fs::read_dir` happens to return them in.
        let td = TestDir::new("find-greatest");
        let dir = td.path().join("rotated");
        fs::create_dir_all(&dir).expect("mkdirs");
        let dotted = dir.join("changelog.md");
        let dashed = dir.join("changelog-20260417.md");
        fs::write(&dotted, b"older").expect("seed dotted");
        fs::write(&dashed, b"newer").expect("seed dashed");

        let found = find_file_containing(td.path(), "changelog")
            .expect("walk")
            .expect("hit");

        // Re-derive the expected greatest the same way the helper
        // does, so the test stays correct regardless of human
        // intuition about ASCII ordering of '-' vs '.'.
        let mut expected = [dotted.clone(), dashed.clone()];
        expected.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
        let want = expected.last().unwrap();
        assert_eq!(
            &found, want,
            "find_file_containing must return the lex-greatest match"
        );
    }

    #[test]
    fn assert_usage_nonzero_rejects_missing_input_tokens() {
        // Omits input_tokens entirely; must surface the distinct
        // \"missing or non-numeric\" message rather than silently treating
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
