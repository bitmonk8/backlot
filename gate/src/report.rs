#![allow(dead_code)]
// Scaffolding: report helpers are exercised by tests now and consumed by runner/main wiring later.

//! Result aggregation, summary table formatting, and JSON output.

use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::types::{StageResult, TestOutcome};

/// Sum of all per-test costs across all stages. `None` is treated as 0.
///
/// The sum is normalized so that an empty input (or one whose costs are all
/// `None`) returns positive zero. `<f64 as Sum>::sum` of an empty iterator
/// produces `-0.0`, which would surface as `-0.00` in the formatted summary
/// and as a literal `-0.0` token in the JSON output.
pub fn total_cost(results: &[StageResult]) -> f64 {
    let sum: f64 = results
        .iter()
        .flat_map(|s| s.results.iter())
        .filter_map(|t| t.cost_usd)
        .sum();
    norm_zero(sum)
}

/// Normalize -0.0 to +0.0. See `total_cost` for why this matters.
fn norm_zero(x: f64) -> f64 {
    if x == 0.0 { 0.0_f64 } else { x }
}

/// Sum of all stage durations.
pub fn total_duration(results: &[StageResult]) -> Duration {
    results.iter().map(|s| s.duration).sum()
}

fn format_cost(c: f64) -> String {
    let c = norm_zero(c);
    format!("${c:.2}")
}

fn format_duration(d: Duration) -> String {
    // Round to one decimal place *before* splitting into minutes+seconds,
    // so durations like 119.96s render as "2m 0.0s" rather than "1m 60.0s".
    let secs = (d.as_secs_f64() * 10.0).round() / 10.0;
    if secs >= 60.0 {
        let mins = (secs / 60.0).floor() as u64;
        let rem = secs - (mins as f64) * 60.0;
        format!("{mins}m {rem:.1}s")
    } else {
        format!("{secs:.1}s")
    }
}

const STAGE_W: usize = 8;
const NUM_W: usize = 5;
const COST_W: usize = 7;
const DUR_W: usize = 9;
// Inner-box width chosen to fit Stage(8) + 5 numeric columns(5) + Cost(7) + Duration(9)
// + 7 single-space gaps + 2-space left padding, with comfortable trailing slack.
const INNER_WIDTH: usize = 60;

fn box_line(content: &str, inner: usize) -> String {
    let padded = format!("  {content}");
    let count = padded.chars().count();
    let pad = inner.saturating_sub(count);
    let spaces: String = " ".repeat(pad);
    format!("\u{2551}{padded}{spaces}\u{2551}")
}

#[allow(clippy::too_many_arguments)]
fn row_line(
    stage: &str,
    tests: usize,
    pass: usize,
    fail: usize,
    soft: usize,
    skip: usize,
    cost: f64,
    dur: Duration,
) -> String {
    let body = format!(
        "{:<sw$} {:>nw$} {:>nw$} {:>nw$} {:>nw$} {:>nw$} {:>cw$} {:>dw$}",
        stage,
        tests,
        pass,
        fail,
        soft,
        skip,
        format_cost(cost),
        format_duration(dur),
        sw = STAGE_W,
        nw = NUM_W,
        cw = COST_W,
        dw = DUR_W,
    );
    box_line(&body, INNER_WIDTH)
}

/// Render the human-readable summary table for a completed run.
///
/// Empty input still produces the header/footer and a totals row of zeros.
pub fn format_summary(results: &[StageResult]) -> String {
    let total_tests: usize = results.iter().map(|s| s.results.len()).sum();
    let total_pass: usize = results.iter().map(|s| s.passed()).sum();
    let total_fail: usize = results.iter().map(|s| s.failed()).sum();
    let total_soft: usize = results.iter().map(|s| s.soft_failed()).sum();
    let total_skip: usize = results.iter().map(|s| s.skipped()).sum();
    let total_cost_value = total_cost(results);
    let total_dur = total_duration(results);

    let bar = "\u{2550}".repeat(INNER_WIDTH);
    let top = format!("\u{2554}{bar}\u{2557}");
    let mid = format!("\u{2560}{bar}\u{2563}");
    let bot = format!("\u{255A}{bar}\u{255D}");

    let header_body = format!(
        "{:<sw$} {:>nw$} {:>nw$} {:>nw$} {:>nw$} {:>nw$} {:>cw$} {:>dw$}",
        "Stage",
        "Tests",
        "Pass",
        "Fail",
        "Soft",
        "Skip",
        "Cost",
        "Duration",
        sw = STAGE_W,
        nw = NUM_W,
        cw = COST_W,
        dw = DUR_W,
    );

    let mut out = String::new();
    out.push_str(&top);
    out.push('\n');
    out.push_str(&box_line("Gate \u{2014} End-to-End Results", INNER_WIDTH));
    out.push('\n');
    out.push_str(&mid);
    out.push('\n');
    out.push_str(&box_line(&header_body, INNER_WIDTH));
    out.push('\n');

    for s in results {
        let line = row_line(
            &s.stage.to_string(),
            s.results.len(),
            s.passed(),
            s.failed(),
            s.soft_failed(),
            s.skipped(),
            s.total_cost(),
            s.duration,
        );
        out.push_str(&line);
        out.push('\n');
    }

    out.push_str(&mid);
    out.push('\n');
    out.push_str(&row_line(
        "Total",
        total_tests,
        total_pass,
        total_fail,
        total_soft,
        total_skip,
        total_cost_value,
        total_dur,
    ));
    out.push('\n');
    out.push_str(&bot);
    out.push('\n');
    out
}

fn outcome_str(o: &TestOutcome) -> &'static str {
    match o {
        TestOutcome::Pass => "pass",
        TestOutcome::Fail(_) => "fail",
        TestOutcome::Skip(_) => "skip",
        TestOutcome::SoftFail(_) => "soft_fail",
    }
}

fn outcome_detail(o: &TestOutcome) -> Option<&str> {
    match o {
        TestOutcome::Pass => None,
        TestOutcome::Fail(s) | TestOutcome::Skip(s) | TestOutcome::SoftFail(s) => Some(s.as_str()),
    }
}

fn test_to_json(t: &crate::types::TestResult) -> Value {
    let mut m = Map::new();
    m.insert("test".into(), Value::String(t.test.clone()));
    m.insert(
        "outcome".into(),
        Value::String(outcome_str(&t.outcome).into()),
    );
    if let Some(d) = outcome_detail(&t.outcome) {
        m.insert("detail".into(), Value::String(d.into()));
    }
    m.insert("duration_secs".into(), json!(t.duration.as_secs_f64()));
    if let Some(c) = t.cost_usd {
        m.insert("cost_usd".into(), json!(c));
    }
    if let Some(ti) = t.tokens_in {
        m.insert("tokens_in".into(), json!(ti));
    }
    if let Some(to) = t.tokens_out {
        m.insert("tokens_out".into(), json!(to));
    }
    Value::Object(m)
}

fn stage_to_json(s: &StageResult) -> Value {
    let tests_json: Vec<Value> = s.results.iter().map(test_to_json).collect();
    json!({
        "stage": s.stage.to_string(),
        "tests": tests_json,
        "duration_secs": s.duration.as_secs_f64(),
        "total_cost_usd": norm_zero(s.total_cost()),
    })
}

fn build_json(results: &[StageResult]) -> Value {
    let stages_json: Vec<Value> = results.iter().map(stage_to_json).collect();
    json!({
        "stages": stages_json,
        "total_tests": results.iter().map(|s| s.results.len()).sum::<usize>(),
        "total_passed": results.iter().map(|s| s.passed()).sum::<usize>(),
        "total_failed": results.iter().map(|s| s.failed()).sum::<usize>(),
        "total_skipped": results.iter().map(|s| s.skipped()).sum::<usize>(),
        "total_soft_failed": results.iter().map(|s| s.soft_failed()).sum::<usize>(),
        "total_cost_usd": total_cost(results),
        "total_duration_secs": total_duration(results).as_secs_f64(),
    })
}

/// Write structured JSON results to `path`. Pretty-printed for human review.
pub fn write_results_json(results: &[StageResult], path: &Path) -> std::io::Result<()> {
    let v = build_json(results);
    let s = serde_json::to_string_pretty(&v)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, s)
}

/// Write per-test transcript files into `dir`. For each `TestResult`
/// whose `stdout` / `stderr` field is `Some`, writes
/// `dir/{stage}-{test}.stdout` and `.stderr` respectively. Tests whose
/// captured streams are `None` are skipped (no empty placeholder file).
///
/// `dir` is created if it does not exist. Returns the number of files
/// actually written so callers can log the count; an I/O error is
/// surfaced as `Err` and stops the loop -- there's no partial-success
/// recovery, because a failure to write the first transcript usually
/// means the directory is unwritable and subsequent writes will fail
/// the same way.
///
/// File names use `{stage}-{test}` directly (no escaping). Test names
/// in gate use only `[a-z0-9-]`, so no path-separator characters can
/// sneak in; the runtime check below pins that contract for any future
/// stage that adds a test with an unusual name.
pub fn write_transcripts(results: &[StageResult], dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut written = 0usize;
    for stage in results {
        for t in &stage.results {
            // Defensive sanity check: any stage that ever embeds a path
            // separator in a test name would silently scribble outside
            // `dir`. Surface the bug here as an explicit error rather
            // than letting `std::fs::write` happily create a file in a
            // sibling directory.
            if t.test.contains('/') || t.test.contains('\\') {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "test name {:?} contains a path separator; transcripts cannot be written safely",
                        t.test
                    ),
                ));
            }
            let base = format!("{}-{}", stage.stage, t.test);
            if let Some(out) = &t.stdout {
                std::fs::write(dir.join(format!("{base}.stdout")), out)?;
                written += 1;
            }
            if let Some(err) = &t.stderr {
                std::fs::write(dir.join(format!("{base}.stderr")), err)?;
                written += 1;
            }
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Stage, StageResult, TestOutcome, TestResult};
    use std::time::Duration;
    use tempfile::NamedTempFile;

    fn mk_test(
        stage: Stage,
        name: &str,
        outcome: TestOutcome,
        cost: Option<f64>,
        dur: Duration,
    ) -> TestResult {
        TestResult {
            stage,
            test: name.into(),
            outcome,
            duration: dur,
            cost_usd: cost,
            tokens_in: None,
            tokens_out: None,
            stdout: None,
            stderr: None,
        }
    }

    fn mk_test_with_tokens(
        stage: Stage,
        name: &str,
        outcome: TestOutcome,
        cost: Option<f64>,
        ti: Option<u64>,
        to: Option<u64>,
    ) -> TestResult {
        TestResult {
            stage,
            test: name.into(),
            outcome,
            duration: Duration::from_secs_f64(1.2),
            cost_usd: cost,
            tokens_in: ti,
            tokens_out: to,
            stdout: None,
            stderr: None,
        }
    }

    fn mk_stage(stage: Stage, results: Vec<TestResult>, dur_secs: f64) -> StageResult {
        StageResult {
            stage,
            results,
            duration: Duration::from_secs_f64(dur_secs),
        }
    }

    fn write_and_read(stages: &[StageResult]) -> Value {
        let f = NamedTempFile::new().expect("temp file");
        write_results_json(stages, f.path()).expect("write json");
        let s = std::fs::read_to_string(f.path()).expect("read json");
        serde_json::from_str(&s).expect("parse json")
    }

    fn numeric_tokens(line: &str) -> Vec<&str> {
        line.split_whitespace()
            .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()))
            .collect()
    }

    // ---- format_summary ----

    #[test]
    fn summary_empty() {
        let s = format_summary(&[]);
        assert!(s.contains("Gate \u{2014} End-to-End Results"));
        assert!(s.contains("Total"));
        assert!(s.contains("$0.00"));
        assert!(s.contains("0.0s"));
        // top, title, mid, header, mid, total, bot = 7 lines
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 7, "lines: {lines:?}");
    }

    #[test]
    fn summary_single_stage_all_pass() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![
                mk_test(
                    Stage::Flick,
                    "t1",
                    TestOutcome::Pass,
                    Some(0.01),
                    Duration::from_secs_f64(1.0),
                ),
                mk_test(
                    Stage::Flick,
                    "t2",
                    TestOutcome::Pass,
                    Some(0.01),
                    Duration::from_secs_f64(1.0),
                ),
                mk_test(
                    Stage::Flick,
                    "t3",
                    TestOutcome::Pass,
                    Some(0.0),
                    Duration::from_secs_f64(1.0),
                ),
            ],
            4.2,
        )];
        let s = format_summary(&stages);
        let row = s.lines().find(|l| l.contains("flick")).expect("flick row");
        // tests=3, pass=3, fail=0, soft=0, skip=0
        assert_eq!(
            numeric_tokens(row),
            vec!["3", "3", "0", "0", "0"],
            "row was: {row}"
        );
        assert!(row.contains("$0.02"), "row: {row}");
        assert!(row.contains("4.2s"), "row: {row}");

        let total = s.lines().find(|l| l.contains("Total")).expect("total row");
        assert!(total.contains("$0.02"), "total: {total}");
    }

    #[test]
    fn summary_mixed_outcomes() {
        let stages = vec![
            mk_stage(
                Stage::Flick,
                vec![
                    mk_test(
                        Stage::Flick,
                        "p",
                        TestOutcome::Pass,
                        Some(0.01),
                        Duration::from_secs_f64(1.0),
                    ),
                    mk_test(
                        Stage::Flick,
                        "f",
                        TestOutcome::Fail("x".into()),
                        None,
                        Duration::from_secs_f64(1.0),
                    ),
                ],
                2.0,
            ),
            mk_stage(
                Stage::Lot,
                vec![
                    mk_test(
                        Stage::Lot,
                        "s",
                        TestOutcome::Skip("nope".into()),
                        None,
                        Duration::from_secs_f64(0.0),
                    ),
                    mk_test(
                        Stage::Lot,
                        "sf",
                        TestOutcome::SoftFail("net".into()),
                        None,
                        Duration::from_secs_f64(0.5),
                    ),
                ],
                1.5,
            ),
        ];
        let s = format_summary(&stages);

        let flick_row = s.lines().find(|l| l.contains("flick")).expect("flick row");
        assert_eq!(
            numeric_tokens(flick_row),
            vec!["2", "1", "1", "0", "0"],
            "flick row: {flick_row}"
        );

        let lot_row = s.lines().find(|l| l.contains(" lot ")).expect("lot row");
        assert_eq!(
            numeric_tokens(lot_row),
            vec!["2", "0", "0", "1", "1"],
            "lot row: {lot_row}"
        );

        // Per-stage cost column reflects each stage's own total, not the run total.
        assert!(flick_row.contains("$0.01"), "flick cost: {flick_row}");
        assert!(lot_row.contains("$0.00"), "lot cost: {lot_row}");

        // Per-stage duration column reflects each stage's own duration, not the run total.
        // (Catches a regression where row_line is fed total_dur instead of s.duration.)
        assert!(flick_row.contains("2.0s"), "flick duration: {flick_row}");
        assert!(lot_row.contains("1.5s"), "lot duration: {lot_row}");

        let total = s.lines().find(|l| l.contains("Total")).expect("total row");
        assert_eq!(
            numeric_tokens(total),
            vec!["4", "1", "1", "1", "1"],
            "total: {total}"
        );
        assert!(total.contains("$0.01"), "total cost: {total}");
    }

    #[test]
    fn summary_cost_formatting() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![mk_test(
                Stage::Flick,
                "t",
                TestOutcome::Pass,
                Some(1.234),
                Duration::from_secs_f64(1.0),
            )],
            1.0,
        )];
        let s = format_summary(&stages);
        assert!(s.contains("$1.23"), "summary: {s}");
        let total = s.lines().find(|l| l.contains("Total")).unwrap();
        assert!(total.contains("$1.23"), "total: {total}");
    }

    #[test]
    fn summary_duration_seconds() {
        // 4.25s is exactly representable in f64, and `f64::round()` is
        // round-half-away-from-zero (IEEE 754), so the formatted output is
        // deterministic.
        let stages = vec![mk_stage(Stage::Flick, vec![], 4.25)];
        let s = format_summary(&stages);
        let flick = s.lines().find(|l| l.contains("flick")).unwrap();
        assert!(flick.contains("4.3s"), "row: {flick}");
        assert!(!flick.contains("0m"), "should not show minutes: {flick}");
    }

    #[test]
    fn summary_duration_minutes() {
        let stages = vec![mk_stage(Stage::Flick, vec![], 125.3)];
        let s = format_summary(&stages);
        let flick = s.lines().find(|l| l.contains("flick")).unwrap();
        assert!(flick.contains("2m"), "row: {flick}");
        assert!(flick.contains("5.3s"), "row: {flick}");
    }

    #[test]
    fn summary_soft_fail_not_in_fail_column() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![mk_test(
                Stage::Flick,
                "sf",
                TestOutcome::SoftFail("net".into()),
                None,
                Duration::from_secs_f64(1.0),
            )],
            1.0,
        )];
        let s = format_summary(&stages);
        let header = s
            .lines()
            .find(|l| l.contains("Stage") && l.contains("Tests"))
            .expect("header row");
        assert!(
            header.contains("Soft"),
            "header should include Soft column: {header}"
        );

        let row = s.lines().find(|l| l.contains("flick")).expect("flick row");
        // tests=1, pass=0, fail=0, soft=1, skip=0
        assert_eq!(
            numeric_tokens(row),
            vec!["1", "0", "0", "1", "0"],
            "row: {row}"
        );
    }

    // ---- write_results_json ----

    #[test]
    fn json_round_trip() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![mk_test_with_tokens(
                Stage::Flick,
                "basic-invocation",
                TestOutcome::Pass,
                Some(0.003),
                Some(150),
                Some(42),
            )],
            4.2,
        )];
        let v = write_and_read(&stages);
        assert_eq!(v["total_tests"], json!(1));
        assert_eq!(v["total_passed"], json!(1));
        assert_eq!(v["total_failed"], json!(0));
        assert_eq!(v["total_skipped"], json!(0));
        assert_eq!(v["total_soft_failed"], json!(0));
        assert_eq!(v["stages"][0]["stage"], json!("flick"));
        assert_eq!(
            v["stages"][0]["tests"][0]["test"],
            json!("basic-invocation")
        );
        assert_eq!(v["stages"][0]["tests"][0]["outcome"], json!("pass"));
        assert_eq!(v["stages"][0]["tests"][0]["tokens_in"], json!(150));
        assert_eq!(v["stages"][0]["tests"][0]["tokens_out"], json!(42));
        assert_eq!(v["stages"][0]["tests"][0]["cost_usd"], json!(0.003));
        // Per-test duration_secs is recorded
        let test_dur = v["stages"][0]["tests"][0]["duration_secs"]
            .as_f64()
            .unwrap();
        assert!(
            (test_dur - 1.2).abs() < 1e-9,
            "test duration_secs: {test_dur}"
        );
        // Stage-level duration_secs and total_cost_usd are recorded
        let stage_dur = v["stages"][0]["duration_secs"].as_f64().unwrap();
        assert!(
            (stage_dur - 4.2).abs() < 1e-9,
            "stage duration_secs: {stage_dur}"
        );
        let stage_cost = v["stages"][0]["total_cost_usd"].as_f64().unwrap();
        assert!(
            (stage_cost - 0.003).abs() < 1e-9,
            "stage total_cost_usd: {stage_cost}"
        );
    }

    #[test]
    fn json_outcome_strings() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![
                mk_test(Stage::Flick, "p", TestOutcome::Pass, None, Duration::ZERO),
                mk_test(
                    Stage::Flick,
                    "f",
                    TestOutcome::Fail("x".into()),
                    None,
                    Duration::ZERO,
                ),
                mk_test(
                    Stage::Flick,
                    "s",
                    TestOutcome::Skip("y".into()),
                    None,
                    Duration::ZERO,
                ),
                mk_test(
                    Stage::Flick,
                    "sf",
                    TestOutcome::SoftFail("z".into()),
                    None,
                    Duration::ZERO,
                ),
            ],
            0.0,
        )];
        let v = write_and_read(&stages);
        let tests = v["stages"][0]["tests"].as_array().unwrap();
        assert_eq!(tests[0]["outcome"], json!("pass"));
        assert_eq!(tests[1]["outcome"], json!("fail"));
        assert_eq!(tests[2]["outcome"], json!("skip"));
        assert_eq!(tests[3]["outcome"], json!("soft_fail"));
        // detail propagates for every non-pass outcome, not just Fail
        assert_eq!(tests[1]["detail"], json!("x"));
        assert_eq!(tests[2]["detail"], json!("y"));
        assert_eq!(tests[3]["detail"], json!("z"));
    }

    #[test]
    fn json_fail_has_detail() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![mk_test(
                Stage::Flick,
                "f",
                TestOutcome::Fail("oops".into()),
                None,
                Duration::ZERO,
            )],
            0.0,
        )];
        let v = write_and_read(&stages);
        assert_eq!(v["stages"][0]["tests"][0]["detail"], json!("oops"));
    }

    #[test]
    fn json_pass_no_detail() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![mk_test(
                Stage::Flick,
                "p",
                TestOutcome::Pass,
                None,
                Duration::ZERO,
            )],
            0.0,
        )];
        let v = write_and_read(&stages);
        let test_obj = &v["stages"][0]["tests"][0];
        assert!(
            test_obj.as_object().unwrap().get("detail").is_none(),
            "Pass outcome should have no detail field: {test_obj}"
        );
    }

    #[test]
    fn json_optional_tokens() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![mk_test(
                Stage::Flick,
                "p",
                TestOutcome::Pass,
                None,
                Duration::ZERO,
            )],
            0.0,
        )];
        let v = write_and_read(&stages);
        let test_obj = v["stages"][0]["tests"][0].as_object().unwrap();
        assert!(
            test_obj.get("tokens_in").is_none(),
            "tokens_in present: {test_obj:?}"
        );
        assert!(
            test_obj.get("tokens_out").is_none(),
            "tokens_out present: {test_obj:?}"
        );
        assert!(
            test_obj.get("cost_usd").is_none(),
            "cost_usd present: {test_obj:?}"
        );
    }

    #[test]
    fn json_totals_correct() {
        let stages = vec![
            mk_stage(
                Stage::Flick,
                vec![
                    mk_test(
                        Stage::Flick,
                        "p1",
                        TestOutcome::Pass,
                        Some(0.10),
                        Duration::from_secs_f64(1.0),
                    ),
                    mk_test(
                        Stage::Flick,
                        "f1",
                        TestOutcome::Fail("x".into()),
                        Some(0.05),
                        Duration::from_secs_f64(1.0),
                    ),
                ],
                3.0,
            ),
            mk_stage(
                Stage::Lot,
                vec![
                    mk_test(
                        Stage::Lot,
                        "s1",
                        TestOutcome::Skip("y".into()),
                        None,
                        Duration::ZERO,
                    ),
                    mk_test(
                        Stage::Lot,
                        "sf1",
                        TestOutcome::SoftFail("z".into()),
                        Some(0.02),
                        Duration::ZERO,
                    ),
                    mk_test(Stage::Lot, "p2", TestOutcome::Pass, None, Duration::ZERO),
                ],
                2.0,
            ),
        ];
        let v = write_and_read(&stages);
        assert_eq!(v["total_tests"], json!(5));
        assert_eq!(v["total_passed"], json!(2));
        assert_eq!(v["total_failed"], json!(1));
        assert_eq!(v["total_skipped"], json!(1));
        assert_eq!(v["total_soft_failed"], json!(1));
        let cost = v["total_cost_usd"].as_f64().unwrap();
        assert!((cost - 0.17).abs() < 1e-9, "total cost: {cost}");
        let dur = v["total_duration_secs"].as_f64().unwrap();
        assert!((dur - 5.0).abs() < 1e-9, "total dur: {dur}");
    }

    // ---- aggregation helpers ----

    #[test]
    fn total_cost_with_nones() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![
                mk_test(
                    Stage::Flick,
                    "a",
                    TestOutcome::Pass,
                    Some(0.10),
                    Duration::ZERO,
                ),
                mk_test(Stage::Flick, "b", TestOutcome::Pass, None, Duration::ZERO),
                mk_test(
                    Stage::Flick,
                    "c",
                    TestOutcome::Pass,
                    Some(0.05),
                    Duration::ZERO,
                ),
            ],
            0.0,
        )];
        let c = total_cost(&stages);
        assert!((c - 0.15).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn total_cost_all_none() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![
                mk_test(Stage::Flick, "a", TestOutcome::Pass, None, Duration::ZERO),
                mk_test(Stage::Flick, "b", TestOutcome::Pass, None, Duration::ZERO),
            ],
            0.0,
        )];
        let c = total_cost(&stages);
        assert_eq!(c, 0.0);
        // The all-None case must return +0.0, not -0.0; that is the contract
        // norm_zero exists to enforce. assert_eq! does not catch a sign flip
        // because IEEE 754 defines -0.0 == 0.0.
        assert!(c.is_sign_positive(), "expected +0.0, got {c}");
    }

    #[test]
    fn total_duration_sums() {
        let stages = vec![
            mk_stage(Stage::Flick, vec![], 1.5),
            mk_stage(Stage::Lot, vec![], 2.5),
            mk_stage(Stage::Reel, vec![], 0.25),
        ];
        let d = total_duration(&stages);
        assert!(
            (d.as_secs_f64() - 4.25).abs() < 1e-9,
            "got {} secs",
            d.as_secs_f64()
        );
    }

    // ---- format_cost / format_duration direct tests ----

    #[test]
    fn format_cost_zero_normalizes_negative_zero() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(-0.0), "$0.00");
        let empty_sum: f64 = std::iter::empty::<f64>().sum();
        assert_eq!(format_cost(empty_sum), "$0.00");
    }

    #[test]
    fn format_cost_basic() {
        assert_eq!(format_cost(1.234), "$1.23");
        assert_eq!(format_cost(0.10), "$0.10");
        assert_eq!(format_cost(99.5), "$99.50");
    }

    #[test]
    fn format_duration_seconds_branch() {
        assert_eq!(format_duration(Duration::from_secs_f64(0.0)), "0.0s");
        assert_eq!(format_duration(Duration::from_secs_f64(0.04)), "0.0s");
        assert_eq!(format_duration(Duration::from_secs_f64(4.2)), "4.2s");
        assert_eq!(format_duration(Duration::from_secs_f64(59.94)), "59.9s");
    }

    #[test]
    fn format_duration_minute_boundary_rounds_up() {
        // Values just below a minute round up across the boundary; without the
        // round-then-split ordering they would render "0m 60.0s" / "1m 60.0s".
        assert_eq!(format_duration(Duration::from_secs_f64(59.96)), "1m 0.0s");
        assert_eq!(format_duration(Duration::from_secs_f64(60.0)), "1m 0.0s");
        assert_eq!(format_duration(Duration::from_secs_f64(119.96)), "2m 0.0s");
    }

    #[test]
    fn format_duration_minutes_branch() {
        assert_eq!(format_duration(Duration::from_secs_f64(125.3)), "2m 5.3s");
        assert_eq!(format_duration(Duration::from_secs_f64(600.0)), "10m 0.0s");
    }

    #[test]
    fn json_total_cost_is_positive_zero_when_all_none() {
        let stages = vec![mk_stage(
            Stage::Flick,
            vec![
                mk_test(Stage::Flick, "a", TestOutcome::Pass, None, Duration::ZERO),
                mk_test(Stage::Flick, "b", TestOutcome::Pass, None, Duration::ZERO),
            ],
            0.0,
        )];
        // Round-trip parse: parsed value must equal +0.0
        let v = write_and_read(&stages);
        let total = v["total_cost_usd"].as_f64().unwrap();
        assert_eq!(total, 0.0);
        let stage_total = v["stages"][0]["total_cost_usd"].as_f64().unwrap();
        assert_eq!(stage_total, 0.0);

        // Raw serialized text must not contain a literal "-0" sign for cost fields.
        let f = NamedTempFile::new().unwrap();
        write_results_json(&stages, f.path()).unwrap();
        let raw = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            !raw.contains("-0.0"),
            "expected no \"-0.0\" tokens in JSON output:
{raw}"
        );
    }

    #[test]
    fn write_results_json_empty_stages_produces_valid_structure() {
        let f = NamedTempFile::new().expect("temp");
        write_results_json(&[], f.path()).expect("write empty");
        let raw = std::fs::read_to_string(f.path()).expect("read");
        let v: Value = serde_json::from_str(&raw).expect("parse");

        // All required top-level keys present with correct zero values.
        assert_eq!(v["stages"], json!([]));
        assert_eq!(v["total_tests"], json!(0));
        assert_eq!(v["total_passed"], json!(0));
        assert_eq!(v["total_failed"], json!(0));
        assert_eq!(v["total_skipped"], json!(0));
        assert_eq!(v["total_soft_failed"], json!(0));
        assert_eq!(v["total_cost_usd"].as_f64().unwrap(), 0.0);
        assert_eq!(v["total_duration_secs"].as_f64().unwrap(), 0.0);
    }
    #[test]
    fn write_results_json_returns_err_for_unwritable_path() {
        // Path with a non-existent parent directory cannot be created by std::fs::write.
        let bad = std::path::PathBuf::from("target/gate-scratch/does/not/exist/results.json");
        // Make sure it really does not exist (no leftover from prior runs).
        let _ = std::fs::remove_file(&bad);
        let stages: Vec<StageResult> = vec![];
        let result = write_results_json(&stages, &bad);
        assert!(result.is_err(), "expected Err for unwritable path, got Ok");
    }

    fn mk_test_with_streams(
        stage: Stage,
        name: &str,
        stdout: Option<&str>,
        stderr: Option<&str>,
    ) -> TestResult {
        TestResult {
            stage,
            test: name.into(),
            outcome: TestOutcome::Pass,
            duration: Duration::ZERO,
            cost_usd: None,
            tokens_in: None,
            tokens_out: None,
            stdout: stdout.map(str::to_string),
            stderr: stderr.map(str::to_string),
        }
    }

    /// Spec TDD test #7: when test results carry captured streams, the
    /// helper writes one `{stage}-{test}.stdout` and `.stderr` file per
    /// populated stream, and skips files whose stream is `None`.
    #[test]
    fn write_transcripts_writes_files_with_correct_names() {
        // Project-local scratch under `target/gate-scratch/` for
        // consistency with sibling tests in this module (and the
        // workspace-wide preference against system temp).
        let parent = std::path::PathBuf::from("target/gate-scratch")
            .join(format!("transcripts-write-{}", std::process::id()));
        let dir = parent.join("ts");
        let _ = std::fs::remove_dir_all(&parent);
        let stages = vec![mk_stage(
            Stage::Epic,
            vec![
                mk_test_with_streams(
                    Stage::Epic,
                    "leaf-task",
                    Some(
                        "hello
",
                    ),
                    Some(
                        "warn
",
                    ),
                ),
                mk_test_with_streams(Stage::Epic, "status", Some("only-stdout"), None),
                mk_test_with_streams(Stage::Epic, "resume-completed", None, Some("only-stderr")),
            ],
            0.0,
        )];
        let count = write_transcripts(&stages, &dir).expect("write transcripts");
        assert_eq!(count, 4, "expected 4 transcript files, wrote {count}");

        // Files present with exact names.
        for (name, want) in [
            (
                "epic-leaf-task.stdout",
                "hello
",
            ),
            (
                "epic-leaf-task.stderr",
                "warn
",
            ),
            ("epic-status.stdout", "only-stdout"),
            ("epic-resume-completed.stderr", "only-stderr"),
        ] {
            let p = dir.join(name);
            assert!(p.is_file(), "expected file {} to exist", p.display());
            let body = std::fs::read_to_string(&p).expect("read transcript");
            assert_eq!(body, want, "{}", p.display());
        }
        // Skipped files are absent.
        for name in ["epic-status.stderr", "epic-resume-completed.stdout"] {
            assert!(
                !dir.join(name).exists(),
                "did not expect {} to exist (no captured stream)",
                name
            );
        }
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn write_transcripts_creates_dir_when_missing() {
        // Even with zero results, the target dir is created so the
        // runner can rely on its existence.
        let parent = std::path::PathBuf::from("target/gate-scratch")
            .join(format!("transcripts-mkdir-{}", std::process::id()));
        let target = parent.join("nested-transcripts");
        let _ = std::fs::remove_dir_all(&parent);
        let count = write_transcripts(&[], &target).expect("create dir");
        assert_eq!(count, 0);
        assert!(target.is_dir(), "{} should exist", target.display());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn write_transcripts_rejects_test_name_with_path_separator() {
        let parent = std::path::PathBuf::from("target/gate-scratch")
            .join(format!("transcripts-reject-{}", std::process::id()));
        let target = parent.join("ts");
        let _ = std::fs::remove_dir_all(&parent);
        let stages = vec![mk_stage(
            Stage::Epic,
            vec![mk_test_with_streams(
                Stage::Epic,
                "bad/name",
                Some("x"),
                None,
            )],
            0.0,
        )];
        let err = write_transcripts(&stages, &target).expect_err("path sep rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        let _ = std::fs::remove_dir_all(&parent);
    }
}
