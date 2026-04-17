//! lot stage -- eight tests exercising the lot CLI's sandbox enforcement.
//!
//! Each test selects a per-platform policy fixture from
//! `gate/fixtures/lot/<platform>/<test>.yaml`, sets `GATE_SCRATCH` so
//! lot's `${GATE_SCRATCH}` placeholder expansion resolves to the
//! per-stage scratch dir, and spawns a small platform-native command
//! inside the sandbox.
//!
//! `network-allowed` is the only test that reports `SoftFail` on
//! failure: a corporate firewall blocking outbound traffic is an
//! infrastructure issue, not a sandbox defect. Every other test reports
//! a hard `Fail` on assertion failure.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::check::{
    TestFailure, assert_contains, assert_exit_fail, assert_exit_ok, assert_path_exists,
};
use crate::exec::run_command;
use crate::runner::StageContext;
use crate::types::{CommandResult, Stage, TestOutcome, TestResult};

/// Per-test wall-clock cap for the parent `lot run` invocation. Long
/// enough to absorb sandbox-setup overhead on slow CI hosts; short
/// enough that a hung child (without `--timeout` set) still surfaces.
/// The `timeout` test sets its own much-shorter `--timeout` flag.
const LOT_TIMEOUT: Duration = Duration::from_secs(60);

/// `--timeout` flag value passed to `lot run` for the `timeout` test.
/// Short enough that the test completes quickly; long enough that a
/// well-behaved `sleep`/`ping` child still gets started before the
/// timer fires.
const TIMEOUT_TEST_LIMIT_SECS: u64 = 2;

/// Network endpoint hit by the `network-allowed` and `network-denied`
/// tests. Keep it on a stable, low-traffic anchor URL so a 404 page or
/// CDN flap on a heavier endpoint cannot mask a sandbox bug.
const NETWORK_PROBE_HOST: &str = "example.com";

pub fn run(ctx: &StageContext) -> Vec<TestResult> {
    vec![
        test_probe(ctx),
        test_setup_check(ctx),
        test_fs_read_allowed(ctx),
        test_fs_write_allowed(ctx),
        test_fs_deny_overrides_read(ctx),
        test_network_denied(ctx),
        test_network_allowed(ctx),
        test_timeout(ctx),
    ]
}

/// Resolve the per-platform policy fixture for a test. The fixture
/// loader follows `cfg!(target_os)` exactly: a Windows build picks
/// `fixtures/lot/windows/<name>.yaml`, macOS picks `macos/`, every
/// other Unix picks `linux/`. Tests that don't take a policy (`probe`,
/// `setup-check`) do not call this.
fn lot_policy_path(name: &str) -> PathBuf {
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("lot")
        .join(platform)
        .join(format!("{name}.yaml"))
}

/// Spawn lot with the given args. `GATE_SCRATCH` is always exported so
/// `${GATE_SCRATCH}` placeholders in policy YAMLs resolve to the
/// per-stage scratch dir. Lot's own `--timeout` flag is independent of
/// our `LOT_TIMEOUT` cap; the cap is the parent's wall-clock guard.
fn run_lot(ctx: &StageContext, args: &[&str]) -> std::io::Result<CommandResult> {
    let bin = ctx.binaries.lot.to_string_lossy().to_string();
    let scratch = ctx.scratch_dir.to_string_lossy().to_string();
    run_command(&bin, args, None, &[("GATE_SCRATCH", &scratch)], LOT_TIMEOUT)
}

/// Build a `TestResult` from a closure that returns either a body
/// outcome (Pass / SoftFail) or a `TestFailure`. Centralizes the
/// duration-recording and outcome-conversion boilerplate.
fn run_test<F>(name: &str, body: F) -> TestResult
where
    F: FnOnce() -> Result<BodyOutcome, TestFailure>,
{
    let start = Instant::now();
    let outcome = match body() {
        Ok(BodyOutcome::Pass) => TestOutcome::Pass,
        Ok(BodyOutcome::SoftFail(reason)) => TestOutcome::SoftFail(reason),
        Err(f) => TestOutcome::Fail(f.detail),
    };
    TestResult {
        stage: Stage::Lot,
        test: name.into(),
        outcome,
        duration: start.elapsed(),
        cost_usd: None,
        tokens_in: None,
        tokens_out: None,
    }
}

/// What a passing test body can return: a clean Pass, or a SoftFail
/// with reason. Hard failures are surfaced via the `Err(TestFailure)`
/// branch of the body.
enum BodyOutcome {
    Pass,
    SoftFail(String),
}

/// Platform-native command that prints a file's contents to stdout.
/// Used by `fs-read-allowed` and `fs-deny-overrides-read`.
fn cat_command(path: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        // `cmd /c type` because `type` is a cmd builtin, not a separate
        // exe -- AppContainer policies that grant cmd.exe still cover it.
        ("cmd".into(), vec!["/c".into(), "type".into(), path.into()])
    } else {
        ("cat".into(), vec![path.into()])
    }
}

/// Platform-native command that writes a string to a file.
/// Used by `fs-write-allowed`.
fn write_file_command(path: &str, content: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        // `>` redirection requires a shell; pass the whole pipeline as
        // one argument to cmd's `/c`.
        (
            "cmd".into(),
            vec!["/c".into(), format!("echo {content}>{path}")],
        )
    } else {
        (
            "sh".into(),
            vec!["-c".into(), format!("echo '{content}' > '{path}'")],
        )
    }
}

/// Platform-native command that attempts an outbound TCP connection.
/// Used by `network-allowed` and `network-denied`. Curl ships with
/// Windows 10+ and every modern Unix, so a single invocation works
/// everywhere; the only platform divergence is the binary name on
/// disk. `--max-time 5` keeps a hung TCP handshake from stalling the
/// 60-second LOT_TIMEOUT.
fn network_command(host: &str) -> (String, Vec<String>) {
    let bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    (
        bin.into(),
        vec![
            "-sS".into(),
            "--max-time".into(),
            "5".into(),
            format!("https://{host}/"),
        ],
    )
}

/// Platform-native command that runs forever (used by the timeout test).
fn long_running_command() -> (String, Vec<String>) {
    if cfg!(windows) {
        // 100 pings at the default 1s interval = ~100s, well past the
        // 2s `--timeout` we hand to lot.
        (
            "ping".into(),
            vec!["-n".into(), "100".into(), "127.0.0.1".into()],
        )
    } else {
        ("sleep".into(), vec!["100".into()])
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

fn test_probe(ctx: &StageContext) -> TestResult {
    let label = "probe";
    run_test(label, || {
        let out = run_lot(ctx, &["probe"]).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        // `lot probe` prints `<capability>=<true|false>` lines for
        // appcontainer / job_objects / namespaces / seccomp / seatbelt.
        // Assert the platform-appropriate backend is reported `true`.
        let expected = if cfg!(windows) {
            "appcontainer=true"
        } else if cfg!(target_os = "macos") {
            "seatbelt=true"
        } else {
            "seccomp=true"
        };
        assert_contains(&out.stdout, expected, &format!("{label}/{expected}"))?;
        Ok(BodyOutcome::Pass)
    })
}

fn test_setup_check(ctx: &StageContext) -> TestResult {
    let label = "setup-check";
    run_test(label, || {
        let out = run_lot(ctx, &["setup", "--check"]).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        Ok(BodyOutcome::Pass)
    })
}

fn test_fs_read_allowed(ctx: &StageContext) -> TestResult {
    let label = "fs-read-allowed";
    run_test(label, || {
        let target = ctx.scratch_dir.join("read-target.txt");
        std::fs::write(&target, b"sentinel-read-content").map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("could not seed read target: {e}"),
        })?;

        let policy = lot_policy_path("fs-read");
        let policy_str = policy.to_string_lossy().to_string();
        let target_str = target.to_string_lossy().to_string();
        let (cmd, cmd_args) = cat_command(&target_str);
        let mut args: Vec<String> = vec!["run".into(), "-c".into(), policy_str, "--".into(), cmd];
        args.extend(cmd_args);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_lot(ctx, &arg_refs).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        assert_contains(
            &out.stdout,
            "sentinel-read-content",
            &format!("{label}/content"),
        )?;
        Ok(BodyOutcome::Pass)
    })
}

fn test_fs_write_allowed(ctx: &StageContext) -> TestResult {
    let label = "fs-write-allowed";
    run_test(label, || {
        let target = ctx.scratch_dir.join("written.txt");
        // Start clean so a leftover file from a prior run does not give
        // a false pass.
        let _ = std::fs::remove_file(&target);

        let policy = lot_policy_path("fs-write");
        let policy_str = policy.to_string_lossy().to_string();
        let target_str = target.to_string_lossy().to_string();
        let (cmd, cmd_args) = write_file_command(&target_str, "sentinel-write");
        let mut args: Vec<String> = vec!["run".into(), "-c".into(), policy_str, "--".into(), cmd];
        args.extend(cmd_args);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_lot(ctx, &arg_refs).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_ok(&out, label)?;
        assert_path_exists(&target, &format!("{label}/file-created"))?;
        Ok(BodyOutcome::Pass)
    })
}

fn test_fs_deny_overrides_read(ctx: &StageContext) -> TestResult {
    let label = "fs-deny-overrides-read";
    run_test(label, || {
        // Create the parent dir in scratch and a single file under
        // `denied/` that the policy explicitly forbids. Read on the
        // parent is granted; the deny entry must take precedence.
        let denied_dir = ctx.scratch_dir.join("denied");
        std::fs::create_dir_all(&denied_dir).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("create denied dir: {e}"),
        })?;
        let denied_file = denied_dir.join("secret.txt");
        std::fs::write(&denied_file, b"should-not-be-readable").map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("seed denied file: {e}"),
        })?;

        let policy = lot_policy_path("fs-deny");
        let policy_str = policy.to_string_lossy().to_string();
        let denied_str = denied_file.to_string_lossy().to_string();
        let (cmd, cmd_args) = cat_command(&denied_str);
        let mut args: Vec<String> = vec!["run".into(), "-c".into(), policy_str, "--".into(), cmd];
        args.extend(cmd_args);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_lot(ctx, &arg_refs).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_fail(&out, label)?;
        // Defense in depth: even if the child's exit code lied (some
        // shells swallow errors), the file's content must not appear in
        // stdout.
        if out.stdout.contains("should-not-be-readable") {
            return Err(TestFailure {
                label: label.into(),
                detail: format!(
                    "denied file content leaked to stdout: {:?}",
                    out.stdout.trim()
                ),
            });
        }
        Ok(BodyOutcome::Pass)
    })
}

fn test_network_denied(ctx: &StageContext) -> TestResult {
    let label = "network-denied";
    run_test(label, || {
        let policy = lot_policy_path("network-denied");
        let policy_str = policy.to_string_lossy().to_string();
        let (cmd, cmd_args) = network_command(NETWORK_PROBE_HOST);
        let mut args: Vec<String> = vec!["run".into(), "-c".into(), policy_str, "--".into(), cmd];
        args.extend(cmd_args);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_lot(ctx, &arg_refs).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        assert_exit_fail(&out, label)?;
        Ok(BodyOutcome::Pass)
    })
}

fn test_network_allowed(ctx: &StageContext) -> TestResult {
    let label = "network-allowed";
    run_test(label, || {
        let policy = lot_policy_path("network-allowed");
        let policy_str = policy.to_string_lossy().to_string();
        let (cmd, cmd_args) = network_command(NETWORK_PROBE_HOST);
        let mut args: Vec<String> = vec!["run".into(), "-c".into(), policy_str, "--".into(), cmd];
        args.extend(cmd_args);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_lot(ctx, &arg_refs).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        if out.exit_code == 0 {
            Ok(BodyOutcome::Pass)
        } else {
            // Per spec: a failed outbound connection in the
            // network-allowed test is reported as SoftFail rather than
            // a hard failure -- the sandbox did its job (allow=true);
            // the network itself was unreachable.
            Ok(BodyOutcome::SoftFail(format!(
                "outbound connection to {NETWORK_PROBE_HOST} failed (exit {}); likely a firewall, not a sandbox defect. stderr: {}",
                out.exit_code,
                out.stderr.trim()
            )))
        }
    })
}

fn test_timeout(ctx: &StageContext) -> TestResult {
    let label = "timeout";
    run_test(label, || {
        let policy = lot_policy_path("timeout");
        let policy_str = policy.to_string_lossy().to_string();
        let timeout_str = TIMEOUT_TEST_LIMIT_SECS.to_string();
        let (cmd, cmd_args) = long_running_command();
        let mut args: Vec<String> = vec![
            "run".into(),
            "-c".into(),
            policy_str,
            "--timeout".into(),
            timeout_str,
            "--".into(),
            cmd,
        ];
        args.extend(cmd_args);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        let out = run_lot(ctx, &arg_refs).map_err(|e| TestFailure {
            label: label.into(),
            detail: format!("spawn failed: {e}"),
        })?;
        // Lot's CLI maps timeout-killed children to exit code 124
        // (the conventional `timeout(1)` code). Require exactly that
        // value rather than just non-zero so a bug where the child
        // crashed for an unrelated reason cannot pass this test.
        if out.exit_code != 124 {
            return Err(TestFailure {
                label: label.into(),
                detail: format!(
                    "expected exit code 124 (timeout), got {} (stderr: {:?})",
                    out.exit_code,
                    out.stderr.trim()
                ),
            });
        }
        // Also require the elapsed wall clock to exceed the configured
        // timeout. A child that exited prematurely for an unrelated
        // reason might still report 124 by coincidence; the duration
        // check rules that out.
        let configured = Duration::from_secs(TIMEOUT_TEST_LIMIT_SECS);
        if out.duration < configured {
            return Err(TestFailure {
                label: label.into(),
                detail: format!(
                    "child exited in {:?}, before the configured timeout {:?}",
                    out.duration, configured
                ),
            });
        }
        Ok(BodyOutcome::Pass)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec test: the per-platform fixture loader returns the fixture
    /// path matching the current OS, with the file actually present on
    /// disk. Anything else means a fixture went missing or the platform
    /// dispatch was broken.
    #[test]
    fn platform_fixture_selection() {
        for name in [
            "fs-read",
            "fs-write",
            "fs-deny",
            "network-denied",
            "network-allowed",
            "timeout",
        ] {
            let p = lot_policy_path(name);
            // Path exists on disk (committed fixture).
            assert!(p.is_file(), "fixture missing: {}", p.display());
            // Path contains the platform tag for the current OS, never
            // a different one.
            let s = p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
            if cfg!(windows) {
                assert!(
                    s.contains("/lot/windows/"),
                    "expected windows fixture, got {s}"
                );
                assert!(!s.contains("/lot/macos/"), "{s}");
                assert!(!s.contains("/lot/linux/"), "{s}");
            } else if cfg!(target_os = "macos") {
                assert!(s.contains("/lot/macos/"), "expected macos fixture, got {s}");
                assert!(!s.contains("/lot/windows/"), "{s}");
                assert!(!s.contains("/lot/linux/"), "{s}");
            } else {
                assert!(s.contains("/lot/linux/"), "expected linux fixture, got {s}");
                assert!(!s.contains("/lot/windows/"), "{s}");
                assert!(!s.contains("/lot/macos/"), "{s}");
            }
        }
    }

    #[test]
    fn cat_command_is_platform_appropriate() {
        let (cmd, args) = cat_command("/some/path");
        if cfg!(windows) {
            assert_eq!(cmd, "cmd");
            assert_eq!(args, vec!["/c", "type", "/some/path"]);
        } else {
            assert_eq!(cmd, "cat");
            assert_eq!(args, vec!["/some/path"]);
        }
    }

    #[test]
    fn write_file_command_uses_shell() {
        let (cmd, args) = write_file_command("/p", "hello");
        if cfg!(windows) {
            assert_eq!(cmd, "cmd");
            assert!(args.iter().any(|a| a.contains("hello")), "{args:?}");
        } else {
            assert_eq!(cmd, "sh");
            assert!(args.iter().any(|a| a.contains("hello")), "{args:?}");
        }
    }

    #[test]
    fn long_running_command_runs_well_past_test_timeout() {
        // The command picked must be > TIMEOUT_TEST_LIMIT_SECS so the
        // timeout test deterministically hits the timeout path; if the
        // command happened to exit early on its own the test would
        // produce a false negative.
        let (cmd, args) = long_running_command();
        if cfg!(windows) {
            assert_eq!(cmd, "ping");
            // 100 pings * 1 sec each = 100s, well past the 2s test timeout.
            assert!(args.iter().any(|a| a == "100"), "{args:?}");
        } else {
            assert_eq!(cmd, "sleep");
            assert_eq!(args, vec!["100"]);
        }
    }

    #[test]
    fn network_command_targets_https() {
        let (cmd, args) = network_command("example.com");
        let want_bin = if cfg!(windows) { "curl.exe" } else { "curl" };
        assert_eq!(cmd, want_bin, "per-platform binary name");
        assert!(
            args.iter().any(|a| a.contains("https://example.com")),
            "expected https URL in args, got {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--max-time"),
            "expected --max-time guard, got {args:?}"
        );
    }
}
