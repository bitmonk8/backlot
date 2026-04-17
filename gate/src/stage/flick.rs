//! flick stage -- six tests exercising the flick CLI end-to-end against a
//! real LLM provider.
//!
//! Each test invokes `flick run` with a fixture config, parses the
//! single-line JSON `FlickResult` written to stdout, and runs the
//! assertions described in `specs/gate/D6.md`. Tests are independent;
//! one failing test does not affect the others.
//!
//! Cost is intentionally low: every test uses the cheapest model alias
//! available (`fast`) except `chatcompletions-invocation`, which uses
//! `balanced` to exercise a second provider/API backend.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::check::{TestFailure, assert_exit_fail, assert_exit_ok, assert_json_field};
use crate::exec::run_command;
use crate::runner::StageContext;
use crate::types::{CommandResult, Stage, TestOutcome, TestResult};

/// Per-test wall-clock cap. Flick API calls usually settle in under 10s;
/// 90s leaves headroom for slow networks without making a hung child
/// invisible.
const FLICK_TIMEOUT: Duration = Duration::from_secs(90);

pub fn run(ctx: &StageContext) -> Vec<TestResult> {
    vec![
        test_basic_invocation(ctx),
        test_chatcompletions_invocation(ctx),
        test_tool_declaration_and_resume(ctx),
        test_structured_output(ctx),
        test_dry_run(ctx),
        test_error_invalid_model(ctx),
    ]
}

/// Resolve the path to a committed flick fixture file. Fixtures live
/// under `gate/fixtures/flick/` so they ship with the source tree.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("flick")
        .join(name)
}

/// Spawn flick with the standard subcommand layout used by every test
/// here: `flick run --config <yaml> [--query <q>] [--dry-run]
/// [--resume <hash> --tool-results <path>]`.
fn run_flick(ctx: &StageContext, args: &[&str]) -> std::io::Result<CommandResult> {
    let bin = ctx.binaries.flick.to_string_lossy().to_string();
    run_command(&bin, args, None, &[], FLICK_TIMEOUT)
}

/// Parse the JSON result line flick prints to stdout. flick emits one
/// line of single-line JSON; parse the whole stdout as a `Value` and
/// surface a `TestFailure` if the bytes are not valid JSON.
fn parse_result(out: &CommandResult, label: &str) -> Result<Value, TestFailure> {
    let trimmed = out.stdout.trim();
    if trimmed.is_empty() {
        return Err(TestFailure {
            label: label.to_string(),
            detail: format!(
                "flick stdout was empty (exit={}, stderr={:?})",
                out.exit_code, out.stderr
            ),
        });
    }
    serde_json::from_str(trimmed).map_err(|e| TestFailure {
        label: label.to_string(),
        detail: format!("could not parse flick stdout as JSON: {e} (raw: {trimmed:?})"),
    })
}

/// Build a `TestResult` from a closure that returns either the body
/// outcome (with optional usage data) or a `TestFailure`. Centralizes
/// the duration-recording and outcome-conversion boilerplate so the
/// per-test functions stay focused on the assertions.
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
        stage: Stage::Flick,
        test: name.into(),
        outcome,
        duration: start.elapsed(),
        cost_usd,
        tokens_in,
        tokens_out,
        stdout: None,
        stderr: None,
    }
}

/// Optional usage data carried back from a passing test body. The
/// per-test functions extract these from the `usage` block of flick's
/// JSON result and pass them through so the summary table and
/// `results.json` carry real numbers, not zeros.
#[derive(Default)]
struct BodyOk {
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
    cost_usd: Option<f64>,
}

/// Pull the `usage` block out of a flick JSON result and convert it to a
/// `BodyOk`. Returns an empty `BodyOk` if `usage` is absent (e.g., the
/// dry-run path); does not fail the test.
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

// ── Tests ──────────────────────────────────────────────────────────────

fn test_basic_invocation(ctx: &StageContext) -> TestResult {
    let label = "basic-invocation";
    run_test(label, || {
        let cfg = fixture_path("basic.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let out = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--query",
                "Reply with the single word 'ready'.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_complete(&json, label)?;
        assert_nonempty_content(&json, label)?;
        assert_usage_block(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_chatcompletions_invocation(ctx: &StageContext) -> TestResult {
    let label = "chatcompletions-invocation";
    run_test(label, || {
        let cfg = fixture_path("balanced.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let out = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--query",
                "Reply with the single word 'ready'.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_complete(&json, label)?;
        assert_nonempty_content(&json, label)?;
        assert_usage_block(&json, label)?;
        Ok(extract_usage(&json))
    })
}

fn test_tool_declaration_and_resume(ctx: &StageContext) -> TestResult {
    let label = "tool-declaration-and-resume";
    run_test(label, || {
        let cfg = fixture_path("tools.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();

        // First call -- expect tool_calls_pending.
        let out1 = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--query",
                "What's the weather in San Francisco? Use the get_weather tool.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("first call spawn failed: {e}"),
        })?;
        assert_exit_ok(&out1, &format!("{label}/call-1"))?;
        let json1 = parse_result(&out1, &format!("{label}/call-1"))?;
        assert_status(&json1, "tool_calls_pending", &format!("{label}/call-1"))?;

        let (tool_use_id, context_hash) = extract_first_tool_use(&json1, label)?;

        // Resume with a synthetic tool result -- expect status=complete.
        let tr_path = ctx.scratch_dir.join("tool-results.json");
        let tr = serde_json::json!([
            {
                "tool_use_id": tool_use_id,
                "content": "Sunny, 72 degrees Fahrenheit",
                "is_error": false
            }
        ]);
        std::fs::write(
            &tr_path,
            serde_json::to_vec(&tr).expect("serialize tool-results"),
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("could not write tool-results file: {e}"),
        })?;

        let tr_str = tr_path.to_string_lossy().to_string();
        let out2 = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--resume",
                &context_hash,
                "--tool-results",
                &tr_str,
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("resume call spawn failed: {e}"),
        })?;
        assert_exit_ok(&out2, &format!("{label}/call-2"))?;
        let json2 = parse_result(&out2, &format!("{label}/call-2"))?;
        assert_status(&json2, "complete", &format!("{label}/call-2"))?;
        assert_nonempty_content(&json2, label)?;

        // Sum the usage from both calls so the reported tokens reflect
        // the full round-trip cost, not just the resume call.
        let u1 = extract_usage(&json1);
        let u2 = extract_usage(&json2);
        Ok(BodyOk {
            tokens_in: sum_opt(u1.tokens_in, u2.tokens_in),
            tokens_out: sum_opt(u1.tokens_out, u2.tokens_out),
            cost_usd: sum_opt_f64(u1.cost_usd, u2.cost_usd),
        })
    })
}

fn test_structured_output(ctx: &StageContext) -> TestResult {
    let label = "structured-output";
    run_test(label, || {
        let cfg = fixture_path("structured.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let out = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--query",
                "Produce a JSON object with name 'widget' and count 3.",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        let json = parse_result(&out, label)?;
        assert_status_complete(&json, label)?;

        // Find the first text block, parse it as JSON, verify the
        // schema's required fields are present with correct primitive
        // types. We do not assert exact values -- the model may
        // capitalize differently or wrap the JSON in a sentence
        // depending on the provider; we only assert the schema contract.
        let text = extract_first_text(&json, label)?;
        let parsed: Value = serde_json::from_str(text.trim()).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("structured output content was not valid JSON: {e} (raw: {text:?})"),
        })?;
        assert_json_field(&parsed, "name", &format!("{label}/name"))?;
        assert_json_field(&parsed, "count", &format!("{label}/count"))?;
        if !parsed.get("count").is_some_and(Value::is_number) {
            return Err(TestFailure {
                label: label.into(),
                detail: format!("'count' is not a number: {:?}", parsed.get("count")),
            });
        }
        Ok(extract_usage(&json))
    })
}

fn test_dry_run(ctx: &StageContext) -> TestResult {
    let label = "dry-run";
    run_test(label, || {
        let cfg = fixture_path("basic.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let out = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--query",
                "this query is never sent to the API",
                "--dry-run",
            ],
        )
        .map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        if out.stdout.trim().is_empty() {
            return Err(TestFailure {
                label: label.into(),
                detail: "expected non-empty stdout (the request payload), got empty".into(),
            });
        }
        // The dry-run output is the request payload, not a FlickResult --
        // it must NOT contain a `usage` field. (FlickResult always carries
        // usage on a real Complete response; dry-run never makes the call.)
        assert_no_usage(&out.stdout, label)?;
        // The payload is a JSON object; verify it round-trips through
        // serde so a future regression emitting plain text is caught.
        let _payload: Value = serde_json::from_str(out.stdout.trim()).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("dry-run output was not JSON: {e} (raw: {:?})", out.stdout),
        })?;
        Ok(BodyOk::default())
    })
}

fn test_error_invalid_model(ctx: &StageContext) -> TestResult {
    let label = "error-invalid-model";
    run_test(label, || {
        let cfg = fixture_path("invalid_model.yaml");
        let cfg_str = cfg.to_string_lossy().to_string();
        let out = run_flick(
            ctx,
            &[
                "run",
                "--config",
                &cfg_str,
                "--query",
                "ignored -- alias resolution fails before any API call",
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

// ── Assertion helpers specific to flick JSON ───────────────────────────

fn assert_status(json: &Value, want: &str, label: &str) -> Result<(), TestFailure> {
    let got = json
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing or non-string `status` in {json:?}"),
        })?;
    if got == want {
        println!("PASS: {label} (status={want})");
        Ok(())
    } else {
        let detail = format!("expected status='{want}', got '{got}'");
        println!("FAIL: {label}: {detail}");
        Err(TestFailure {
            label: label.into(),
            detail,
        })
    }
}

fn assert_status_complete(json: &Value, label: &str) -> Result<(), TestFailure> {
    assert_status(json, "complete", label)
}

fn assert_nonempty_content(json: &Value, label: &str) -> Result<(), TestFailure> {
    let arr = json
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing or non-array `content` in {json:?}"),
        })?;
    if arr.is_empty() {
        return Err(TestFailure {
            label: label.into(),
            detail: "content array is empty".into(),
        });
    }
    println!("PASS: {label}/content (len={})", arr.len());
    Ok(())
}

fn assert_usage_block(json: &Value, label: &str) -> Result<(), TestFailure> {
    assert_json_field(json, "usage", &format!("{label}/usage"))?;
    let u = json.get("usage").expect("verified by assert_json_field");
    assert_json_field(u, "input_tokens", &format!("{label}/usage/input_tokens"))?;
    assert_json_field(u, "output_tokens", &format!("{label}/usage/output_tokens"))?;
    Ok(())
}

fn assert_no_usage(stdout: &str, label: &str) -> Result<(), TestFailure> {
    // Cheap structural check: the dry-run payload is the API request, not
    // a FlickResult, so the "usage" field cannot appear unless flick has
    // accidentally fallen through to the live-call path.
    if stdout.contains("\"usage\"") {
        return Err(TestFailure {
            label: label.into(),
            detail: "dry-run output contained a `usage` field; expected request payload only"
                .into(),
        });
    }
    println!("PASS: {label}/no-usage");
    Ok(())
}

fn extract_first_tool_use(json: &Value, label: &str) -> Result<(String, String), TestFailure> {
    let arr = json
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing content array in {json:?}"),
        })?;
    let id = arr
        .iter()
        .find_map(|b| {
            if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                b.get("id").and_then(Value::as_str).map(str::to_string)
            } else {
                None
            }
        })
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("no ToolUse block in content: {arr:?}"),
        })?;
    let hash = json
        .get("context_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: "missing context_hash on tool_calls_pending result".into(),
        })?
        .to_string();
    // The serde extractor above accepts an empty string as a valid str;
    // assert non-emptiness explicitly so a regression where flick emits
    // "context_hash":"" surfaces here, not as a downstream resume failure.
    if hash.is_empty() {
        let detail = "context_hash present but empty".to_string();
        println!("FAIL: {label}/context-hash-present: {detail}");
        return Err(TestFailure {
            label: label.into(),
            detail,
        });
    }
    println!("PASS: {label}/context-hash-present");
    Ok((id, hash))
}

/// Pull the first `text` block out of the content array. The
/// structured-output test relies on this picking the JSON payload
/// directly; the system prompt in `structured.yaml` instructs the
/// model to emit JSON only, so a leading text block of commentary
/// would itself be a regression worth catching.
fn extract_first_text(json: &Value, label: &str) -> Result<String, TestFailure> {
    let arr = json
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("missing content array in {json:?}"),
        })?;
    arr.iter()
        .find_map(|b| {
            if b.get("type").and_then(Value::as_str) == Some("text") {
                b.get("text").and_then(Value::as_str).map(str::to_string)
            } else {
                None
            }
        })
        .ok_or_else(|| TestFailure {
            label: label.into(),
            detail: format!("no Text block in content: {arr:?}"),
        })
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
        (s, None) | (None, s) => s,
    }
}

fn sum_opt_f64(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (s, None) | (None, s) => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_files_exist() {
        for name in [
            "basic.yaml",
            "balanced.yaml",
            "tools.yaml",
            "structured.yaml",
            "invalid_model.yaml",
        ] {
            let p = fixture_path(name);
            assert!(p.exists(), "fixture missing: {}", p.display());
        }
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
    fn parse_result_rejects_non_json() {
        let cr = CommandResult {
            stdout: "this is not JSON".into(),
            stderr: String::new(),
            exit_code: 0,
            duration: Duration::ZERO,
        };
        let err = parse_result(&cr, "x").expect_err("non-json");
        assert!(err.detail.contains("could not parse"), "{}", err.detail);
    }

    #[test]
    fn parse_result_accepts_complete_result() {
        let cr = CommandResult {
            stdout: r#"{"status":"complete","content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":3,"output_tokens":1,"cost_usd":0.0}}"#.into(),
            stderr: String::new(),
            exit_code: 0,
            duration: Duration::ZERO,
        };
        let v = parse_result(&cr, "x").expect("parse");
        assert_eq!(v["status"], "complete");
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
    fn extract_usage_handles_missing_block() {
        let v: Value = serde_json::from_str(r#"{"status":"complete"}"#).unwrap();
        let u = extract_usage(&v);
        assert!(u.tokens_in.is_none());
        assert!(u.tokens_out.is_none());
        assert!(u.cost_usd.is_none());
    }

    #[test]
    fn extract_first_tool_use_finds_id_and_hash() {
        let v: Value = serde_json::from_str(
            r#"{
            "content":[
                {"type":"text","text":"thinking"},
                {"type":"tool_use","id":"tu_42","name":"x","input":{}}
            ],
            "context_hash":"deadbeef"
        }"#,
        )
        .unwrap();
        let (id, hash) = extract_first_tool_use(&v, "x").unwrap();
        assert_eq!(id, "tu_42");
        assert_eq!(hash, "deadbeef");
    }

    #[test]
    fn extract_first_tool_use_errors_when_absent() {
        let v: Value =
            serde_json::from_str(r#"{"content":[{"type":"text","text":"hi"}]}"#).unwrap();
        let err = extract_first_tool_use(&v, "x").expect_err("no tool use");
        assert!(err.detail.contains("no ToolUse"), "{}", err.detail);
    }

    #[test]
    fn extract_first_tool_use_rejects_empty_context_hash() {
        // Catches a flick regression where the result carries
        // an empty context_hash field instead of a real 32-char hex
        // string; without this guard a downstream resume call would
        // just fail mysteriously inside flick.
        let v: Value = serde_json::from_str(
            r#"{
                "content":[
                    {"type":"tool_use","id":"tu_1","name":"x","input":{}}
                ],
                "context_hash":""
            }"#,
        )
        .unwrap();
        let err = extract_first_tool_use(&v, "x").expect_err("empty hash");
        assert!(err.detail.contains("empty"), "{}", err.detail);
    }

    #[test]
    fn assert_no_usage_rejects_payload_with_usage() {
        let err = assert_no_usage(r#"{"usage":{}}"#, "x").expect_err("had usage");
        assert!(err.detail.contains("usage"), "{}", err.detail);
    }

    #[test]
    fn assert_no_usage_accepts_payload_without_usage() {
        assert_no_usage(r#"{"model":"x","messages":[]}"#, "x").expect("no usage");
    }

    #[test]
    fn sum_opt_handles_none_combinations() {
        assert_eq!(sum_opt(None, None), None);
        assert_eq!(sum_opt(Some(3), None), Some(3));
        assert_eq!(sum_opt(None, Some(4)), Some(4));
        assert_eq!(sum_opt(Some(3), Some(4)), Some(7));
    }

    #[test]
    fn sum_opt_f64_handles_none_combinations() {
        assert_eq!(sum_opt_f64(None, None), None);
        assert_eq!(sum_opt_f64(Some(0.10), None), Some(0.10));
        assert_eq!(sum_opt_f64(None, Some(0.25)), Some(0.25));
        let s = sum_opt_f64(Some(0.10), Some(0.25)).expect("both set");
        assert!((s - 0.35).abs() < 1e-9, "got {s}");
    }
}
