#![allow(dead_code)]
// Scaffolding: assertion helpers are exercised by tests now and consumed by stage modules added later.

//! Assertion helpers for gate stage tests.
//!
//! Each assertion prints a live `PASS:` or `FAIL:` line to stdout for
//! progress feedback and returns `Result<(), TestFailure>` so callers can
//! early-return with `?`.

use std::fmt;
use std::path::Path;

use crate::types::CommandResult;

/// Hard failure raised by an assertion helper.
///
/// Returned by every `assert_*` function so callers can early-return on
/// the first failed check using `?`. The `label` is the short test name
/// supplied by the caller; the `detail` is the assertion-specific reason
/// the check did not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    pub label: String,
    pub detail: String,
}

impl fmt::Display for TestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FAIL: {}: {}", self.label, self.detail)
    }
}

impl std::error::Error for TestFailure {}

fn pass(label: &str) -> Result<(), TestFailure> {
    println!("PASS: {label}");
    Ok(())
}

fn fail(label: &str, detail: String) -> Result<(), TestFailure> {
    println!("FAIL: {label}: {detail}");
    Err(TestFailure {
        label: label.to_string(),
        detail,
    })
}

/// Asserts that the subprocess exited with code 0.
pub fn assert_exit_ok(result: &CommandResult, label: &str) -> Result<(), TestFailure> {
    if result.exit_code == 0 {
        pass(label)
    } else {
        fail(
            label,
            format!("expected exit code 0, got {}", result.exit_code),
        )
    }
}

/// Asserts that the subprocess exited with a non-zero code.
pub fn assert_exit_fail(result: &CommandResult, label: &str) -> Result<(), TestFailure> {
    if result.exit_code != 0 {
        pass(label)
    } else {
        fail(label, "expected non-zero exit code, got 0".to_string())
    }
}

/// Asserts that the JSON value is an object containing `field` and that
/// the field's value is not JSON null.
pub fn assert_json_field(
    json: &serde_json::Value,
    field: &str,
    label: &str,
) -> Result<(), TestFailure> {
    let Some(obj) = json.as_object() else {
        return fail(
            label,
            format!("expected JSON object, got {}", json_kind(json)),
        );
    };
    match obj.get(field) {
        Some(v) if !v.is_null() => pass(label),
        Some(_) => fail(label, format!("field '{field}' is null")),
        None => fail(label, format!("field '{field}' not found in JSON")),
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Asserts that `haystack` contains `needle` (case-sensitive).
pub fn assert_contains(haystack: &str, needle: &str, label: &str) -> Result<(), TestFailure> {
    if haystack.contains(needle) {
        pass(label)
    } else {
        fail(
            label,
            format!(
                "expected to find '{needle}' in output (length {})",
                haystack.len()
            ),
        )
    }
}

/// Asserts that `actual == expected`. Both values are debug-formatted on
/// failure so the diff is visible.
pub fn assert_eq<T: PartialEq + fmt::Debug>(
    actual: &T,
    expected: &T,
    label: &str,
) -> Result<(), TestFailure> {
    if actual == expected {
        pass(label)
    } else {
        fail(label, format!("expected {expected:?}, got {actual:?}"))
    }
}

/// Asserts that `path` exists on disk.
pub fn assert_path_exists(path: &Path, label: &str) -> Result<(), TestFailure> {
    if path.exists() {
        pass(label)
    } else {
        fail(label, format!("path '{}' does not exist", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn cmd(exit_code: i32) -> CommandResult {
        CommandResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code,
            duration: Duration::from_secs(0),
        }
    }

    // ---- Pass cases ----

    #[test]
    fn exit_ok_passes() {
        assert!(assert_exit_ok(&cmd(0), "exit_ok").is_ok());
    }

    #[test]
    fn exit_fail_passes() {
        assert!(assert_exit_fail(&cmd(1), "exit_fail").is_ok());
    }

    #[test]
    fn json_field_passes() {
        let v = serde_json::json!({ "usage": { "in": 5 }, "other": "x" });
        assert!(assert_json_field(&v, "usage", "json_field").is_ok());
    }

    #[test]
    fn contains_passes() {
        assert!(assert_contains("hello world", "world", "contains").is_ok());
    }

    #[test]
    fn eq_passes() {
        assert!(assert_eq(&5, &5, "eq").is_ok());
    }

    #[test]
    fn path_exists_passes() {
        let f = tempfile::NamedTempFile::new().expect("temp file");
        assert!(assert_path_exists(f.path(), "path_exists").is_ok());
    }

    // ---- Fail cases ----

    #[test]
    fn exit_ok_fails() {
        let err = assert_exit_ok(&cmd(1), "exit_ok").unwrap_err();
        assert!(
            err.detail.contains("expected exit code 0"),
            "detail was: {}",
            err.detail
        );
    }

    #[test]
    fn exit_fail_fails() {
        let err = assert_exit_fail(&cmd(0), "exit_fail").unwrap_err();
        assert!(
            err.detail.contains("expected non-zero"),
            "detail was: {}",
            err.detail
        );
    }

    #[test]
    fn json_field_missing() {
        let v = serde_json::json!({ "other": 1 });
        let err = assert_json_field(&v, "usage", "json_field").unwrap_err();
        assert!(err.detail.contains("usage"), "detail was: {}", err.detail);
    }

    #[test]
    fn json_field_null() {
        let v = serde_json::json!({ "usage": null });
        assert!(assert_json_field(&v, "usage", "json_field").is_err());
    }

    #[test]
    fn contains_fails() {
        let err = assert_contains("hello", "world", "contains").unwrap_err();
        assert!(err.detail.contains("world"), "detail was: {}", err.detail);
    }

    #[test]
    fn eq_fails() {
        let err = assert_eq(&3, &5, "eq").unwrap_err();
        assert!(err.detail.contains('3'), "detail was: {}", err.detail);
        assert!(err.detail.contains('5'), "detail was: {}", err.detail);
    }

    #[test]
    fn path_not_exists() {
        let p = PathBuf::from("/definitely/does/not/exist/gate-check-xyz");
        let err = assert_path_exists(&p, "path_exists").unwrap_err();
        assert!(
            err.detail.contains("does/not/exist") || err.detail.contains(r"does\not\exist"),
            "detail was: {}",
            err.detail
        );
    }

    // ---- Edge cases ----

    #[test]
    fn contains_empty_needle() {
        assert!(assert_contains("anything", "", "contains_empty").is_ok());
    }

    #[test]
    fn json_field_on_non_object() {
        let arr = serde_json::json!([1, 2, 3]);
        assert!(assert_json_field(&arr, "x", "non_obj_arr").is_err());
        let s = serde_json::json!("hello");
        assert!(assert_json_field(&s, "x", "non_obj_str").is_err());
    }

    #[test]
    fn exit_ok_negative_code() {
        assert!(assert_exit_ok(&cmd(-1), "exit_ok_neg").is_err());
    }

    // ---- Label propagation ----

    #[test]
    fn failure_contains_label() {
        let err = assert_exit_ok(&cmd(2), "my-label").unwrap_err();
        assert_eq!(err.label, "my-label");
        assert_eq!(
            format!("{err}"),
            "FAIL: my-label: expected exit code 0, got 2"
        );
    }
}
