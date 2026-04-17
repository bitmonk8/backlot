#![allow(dead_code)]
// Scaffolding: subprocess helpers are exercised by tests now and consumed by stage modules added later.

//! Subprocess execution with timeout enforcement.
//!
//! Wraps `std::process::Command` so every gate-issued invocation captures
//! `stdout`, `stderr`, the exit code, and wall-clock duration into a
//! `CommandResult`. A wall-clock timeout kills the child if exceeded; the
//! resulting `CommandResult` reports `exit_code = TIMEOUT_EXIT_CODE` and
//! appends a timeout message to `stderr`.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::types::CommandResult;

/// Sentinel exit code surfaced when no real exit code is available — either
/// (1) gate killed the child because its wall-clock `timeout` expired, or
/// (2) the child exited but `ExitStatus::code()` returned `None` (Unix:
/// killed by a signal; Windows: abnormal-termination forms that don't carry
/// a code). `-1` was chosen because real exit codes are non-negative on
/// every platform gate targets, so a negative value cannot collide. Callers
/// distinguish the two cases via stderr: only the timeout path appends a
/// `gate: command timed out after Xs` line.
pub const TIMEOUT_EXIT_CODE: i32 = -1;

/// Polling interval used while waiting for the child to exit. Short enough
/// that the timeout reaction time is well under one second, long enough
/// that the busy-wait does not chew measurable CPU.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Run `program` with `args`, capturing `stdout`, `stderr`, the exit code,
/// and the wall-clock duration.
///
/// `working_dir` and `env` are applied verbatim. `env` adds (and overrides)
/// individual variables on top of the inherited environment; it does not
/// clear the parent environment.
///
/// If `timeout` elapses before the child exits, the child is killed (and
/// reaped) and the returned `CommandResult` has `exit_code = TIMEOUT_EXIT_CODE`
/// with a `gate: command timed out after Xs` line appended to `stderr`.
///
/// Returns `Err` only for I/O failures spawning or waiting on the child;
/// non-zero exits are reported via `CommandResult::exit_code`, not as `Err`.
pub fn run_command(
    program: &str,
    args: &[&str],
    working_dir: Option<&Path>,
    env: &[(&str, &str)],
    timeout: Duration,
) -> io::Result<CommandResult> {
    run_command_inner(program, args, working_dir, env, timeout, None)
}

/// Variant of [`run_command`] that writes `stdin_bytes` to the child's
/// standard input and then closes the pipe. Used by stages whose CLIs
/// consume their primary input from stdin (for example `vault bootstrap`,
/// which reads requirements text). All other semantics -- timeout
/// enforcement, captured streams, exit-code conventions -- match
/// `run_command` exactly.
///
/// The write happens on a dedicated thread so a child that drains stdin
/// slowly cannot wedge the parent against a full pipe buffer; the writer
/// returns when the bytes are flushed and the pipe is dropped.
pub fn run_command_with_stdin(
    program: &str,
    args: &[&str],
    working_dir: Option<&Path>,
    env: &[(&str, &str)],
    timeout: Duration,
    stdin_bytes: &[u8],
) -> io::Result<CommandResult> {
    run_command_inner(
        program,
        args,
        working_dir,
        env,
        timeout,
        Some(stdin_bytes.to_vec()),
    )
}

fn run_command_inner(
    program: &str,
    args: &[&str],
    working_dir: Option<&Path>,
    env: &[(&str, &str)],
    timeout: Duration,
    stdin_bytes: Option<Vec<u8>>,
) -> io::Result<CommandResult> {
    let start = Instant::now();

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = working_dir {
        cmd.current_dir(d);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;

    // Hand stdin off to a writer thread so a slow child cannot block the
    // parent on a full pipe buffer; the thread closes the pipe (signalling
    // EOF to the child) when done.
    let stdin_handle = match stdin_bytes {
        Some(bytes) => {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("child stdin pipe missing"))?;
            Some(thread::spawn(move || -> io::Result<()> {
                use std::io::Write;
                stdin.write_all(&bytes)?;
                drop(stdin);
                Ok(())
            }))
        }
        None => None,
    };

    // Drain stdout/stderr on background threads so a chatty child cannot
    // wedge itself by filling the pipe buffer while we are still waiting.
    // `Stdio::piped()` above guarantees both fields are `Some`; we still
    // convert the unwrap to an `io::Result` so the function's declared
    // error contract holds even if a future refactor changes the stdio
    // configuration.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout pipe missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr pipe missing"))?;
    let stdout_handle = thread::spawn(move || drain_pipe(stdout));
    let stderr_handle = thread::spawn(move || drain_pipe(stderr));

    let exit_status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if start.elapsed() >= timeout {
                    // Best-effort kill + reap. Errors from either are
                    // intentionally ignored: `kill` may report the child
                    // already exited, and `wait` may fail if some other
                    // mechanism reaped it. Either way the `Child` is
                    // dropped immediately after this block.
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(POLL_INTERVAL);
            }
        }
    };

    let duration = start.elapsed();
    // A panicked drain thread would otherwise be invisible: `unwrap_or_default`
    // would surface as empty captured output. Surface the panic in stderr
    // so test failures and live diagnostics show what actually happened.
    let stdout_buf = stdout_handle
        .join()
        .unwrap_or_else(|_| String::from("gate: stdout drain thread panicked"));
    let mut stderr_buf = stderr_handle
        .join()
        .unwrap_or_else(|_| String::from("gate: stderr drain thread panicked"));
    if let Some(handle) = stdin_handle {
        // A writer-thread panic or write error is surfaced in stderr_buf
        // rather than as `Err`: the child may already have exited
        // successfully (for example, a CLI that ignores its stdin), and
        // raising an `io::Error` would mask the real CommandResult.
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if !stderr_buf.is_empty() && !stderr_buf.ends_with('\n') {
                    stderr_buf.push('\n');
                }
                stderr_buf.push_str(&format!("gate: stdin write failed: {e}\n"));
            }
            Err(_) => {
                if !stderr_buf.is_empty() && !stderr_buf.ends_with('\n') {
                    stderr_buf.push('\n');
                }
                stderr_buf.push_str("gate: stdin write thread panicked\n");
            }
        }
    }

    let exit_code = match exit_status {
        Some(status) => status.code().unwrap_or(TIMEOUT_EXIT_CODE),
        None => {
            if !stderr_buf.is_empty() && !stderr_buf.ends_with('\n') {
                stderr_buf.push('\n');
            }
            stderr_buf.push_str(&format!(
                "gate: command timed out after {:.1}s\n",
                timeout.as_secs_f64()
            ));
            TIMEOUT_EXIT_CODE
        }
    };

    Ok(CommandResult {
        stdout: stdout_buf,
        stderr: stderr_buf,
        exit_code,
        duration,
    })
}

/// Read `r` to end and return the bytes as a UTF-8 string, replacing any
/// invalid byte sequences with U+FFFD. `Vec<u8>` + `from_utf8_lossy` is
/// used instead of `String::read_to_string` because gate is built to invoke
/// arbitrary CLIs whose stderr (especially on Windows, where stderr is
/// often CP1252-encoded) is not guaranteed to be UTF-8 — the strict reader
/// would silently truncate at the first invalid byte. Pipe-broken errors
/// from a child that exited mid-write are expected and intentionally
/// swallowed; whatever bytes were already buffered are returned.
fn drain_pipe<R: Read>(mut r: R) -> String {
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::scratch_base;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Project-local scratch dir for one test. Created under
    /// `target/gate-scratch/exec-tests/` so we never reach into system temp
    /// (consistent with the workspace `CLAUDE.md` rule for sandbox paths).
    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = scratch_base()
                .join("exec-tests")
                .join(format!("{label}-{pid}-{id}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// (program, argv) for a child that runs ~10s while emitting only a
    /// few bytes (so the pipe drains can never block). Long enough to
    /// outlast the 1s timeout used by `timeout_kills_process`.
    fn long_running_command() -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            ("ping", vec!["-n".into(), "11".into(), "127.0.0.1".into()])
        } else {
            ("sleep", vec!["10".into()])
        }
    }

    /// (program, argv) for a single-shell `echo $VAR` round-trip via
    /// `cmd /C` on Windows and `sh -c` on Unix.
    fn echo_env_command(var: &str) -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            ("cmd", vec!["/C".into(), format!("echo %{var}%")])
        } else {
            ("sh", vec!["-c".into(), format!("echo ${var}")])
        }
    }

    /// (program, argv) for a child that exits with a specific non-zero
    /// code — used to verify exact codes survive `status.code()` mapping.
    fn exit_code_command(code: i32) -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            ("cmd", vec!["/C".into(), format!("exit {code}")])
        } else {
            ("sh", vec!["-c".into(), format!("exit {code}")])
        }
    }

    /// (program, argv) for a child that writes one line to stdout and a
    /// distinct line to stderr — used to verify the two streams stay
    /// separate end-to-end.
    fn split_streams_command(out_text: &str, err_text: &str) -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd",
                vec![
                    "/C".into(),
                    format!("echo {out_text}& echo {err_text} 1>&2"),
                ],
            )
        } else {
            (
                "sh",
                vec![
                    "-c".into(),
                    format!("printf '%s\n' {out_text}; printf '%s\n' {err_text} >&2"),
                ],
            )
        }
    }

    fn args_as_refs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    // ---- Spec TDD checklist ----

    #[test]
    fn capture_stdout() {
        let r = run_command("git", &["--version"], None, &[], Duration::from_secs(30))
            .expect("git --version runs");
        assert_eq!(r.exit_code, 0, "stderr was: {}", r.stderr);
        assert!(
            r.stdout.contains("git version"),
            "stdout did not contain 'git version': {:?}",
            r.stdout
        );
    }

    #[test]
    fn capture_stderr_on_failure() {
        // `git definitely-not-a-subcommand` exits non-zero and writes a
        // diagnostic to stderr on every supported git version.
        let r = run_command(
            "git",
            &["definitely-not-a-real-subcommand"],
            None,
            &[],
            Duration::from_secs(30),
        )
        .expect("git runs");
        assert_ne!(
            r.exit_code, 0,
            "expected non-zero exit, stdout: {}",
            r.stdout
        );
        assert!(
            !r.stderr.is_empty(),
            "expected non-empty stderr, got empty (stdout: {:?})",
            r.stdout
        );
    }

    #[test]
    fn exit_code_captured() {
        // Verify a specific non-zero exit code is preserved verbatim. The
        // pure success path is already covered by `capture_stdout`; this
        // pins the value-preservation contract on the
        // `status.code() => Some(n)` branch with a non-trivial `n`.
        let (program, args) = exit_code_command(7);
        let arg_refs = args_as_refs(&args);
        let r = run_command(program, &arg_refs, None, &[], Duration::from_secs(30))
            .expect("exit-7 command runs");
        assert_eq!(r.exit_code, 7, "stderr: {}", r.stderr);
    }

    #[test]
    fn duration_recorded() {
        let r = run_command("git", &["--version"], None, &[], Duration::from_secs(30))
            .expect("git --version runs");
        assert!(
            r.duration > Duration::ZERO,
            "duration should be > 0, got {:?}",
            r.duration
        );
    }

    #[test]
    fn working_dir_respected() {
        let dir = TestDir::new("workdir");
        let r = run_command(
            "git",
            &["init"],
            Some(dir.path()),
            &[],
            Duration::from_secs(30),
        )
        .expect("git init runs");
        assert_eq!(
            r.exit_code, 0,
            "git init failed: stdout={:?} stderr={:?}",
            r.stdout, r.stderr
        );
        assert!(
            dir.path().join(".git").is_dir(),
            ".git was not created in working_dir {}",
            dir.path().display()
        );
    }

    #[test]
    fn env_vars_passed() {
        let (program, args) = echo_env_command("GATE_TEST_FOO");
        let arg_refs = args_as_refs(&args);
        let r = run_command(
            program,
            &arg_refs,
            None,
            &[("GATE_TEST_FOO", "gate-bar")],
            Duration::from_secs(30),
        )
        .expect("echo command runs");
        assert_eq!(r.exit_code, 0, "stderr: {}", r.stderr);
        assert!(
            r.stdout.contains("gate-bar"),
            "expected stdout to contain 'gate-bar', got {:?}",
            r.stdout
        );
    }

    #[test]
    fn timeout_kills_process() {
        let (program, args) = long_running_command();
        let arg_refs = args_as_refs(&args);
        let timeout = Duration::from_secs(1);
        let start = Instant::now();
        let r = run_command(program, &arg_refs, None, &[], timeout).expect("spawn long cmd");
        let elapsed = start.elapsed();

        assert_eq!(
            r.exit_code, TIMEOUT_EXIT_CODE,
            "expected sentinel exit code on timeout, got {}",
            r.exit_code
        );
        assert!(
            r.stderr.contains("timed out"),
            "expected stderr to mention timeout, got {:?}",
            r.stderr
        );
        // Tight upper bound: kill should fire within roughly one
        // POLL_INTERVAL of the timeout. 2s is generous for slow CI hosts
        // but tight enough that a regression to multi-second polling
        // would be caught here.
        assert!(
            elapsed < Duration::from_secs(2),
            "run_command did not return promptly after timeout: {elapsed:?}"
        );
        assert!(
            r.duration >= timeout,
            "recorded duration ({:?}) should be at least the timeout ({:?})",
            r.duration,
            timeout
        );
    }

    #[test]
    fn timeout_not_triggered() {
        let timeout = Duration::from_secs(30);
        let r = run_command("git", &["--version"], None, &[], timeout).expect("git --version runs");
        assert_eq!(r.exit_code, 0);
        assert!(
            !r.stderr.contains("timed out"),
            "fast command should not record a timeout, got: {:?}",
            r.stderr
        );
        // The interesting signal: the wait loop returned on `try_wait` ==
        // `Some(_)`, not on the elapsed-time check. Anything close to the
        // configured timeout would mean the polling deadline fired by
        // accident on a sub-second command.
        assert!(
            r.duration < Duration::from_secs(5),
            "fast command should return well below the {timeout:?} timeout, got {:?}",
            r.duration
        );
    }

    // ---- Beyond the spec's TDD list: boundary tests the spec implies
    // but does not enumerate (env negative case, working_dir failure,
    // sub-poll-interval timeouts, stream separation, spawn-not-found).

    #[test]
    fn spawn_failure_returns_err() {
        let r = run_command(
            "this-binary-definitely-does-not-exist-gate",
            &[],
            None,
            &[],
            Duration::from_secs(5),
        );
        let err = r.expect_err("expected spawn failure, got Ok");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "expected NotFound, got {err:?}"
        );
    }

    #[test]
    fn working_dir_missing_returns_err() {
        // Spawning into a non-existent working_dir must fail with an
        // `io::Error` rather than producing a CommandResult with stale
        // state. Build the bogus path under `scratch_base()` so it is an
        // absolute path on every platform — a hard-coded `C:/...` would
        // be a possibly-existing relative path on Unix and could falsely
        // pass for an unrelated reason.
        let bogus = scratch_base().join("definitely-does-not-exist-gate-test-bogus-workdir");
        let r = run_command(
            "git",
            &["--version"],
            Some(&bogus),
            &[],
            Duration::from_secs(5),
        );
        let err = r.expect_err("expected Err for missing working_dir");
        // The exact ErrorKind is platform-dependent: Unix returns
        // `NotFound`, Windows can return `NotADirectory` (error 267 from
        // `SetCurrentDirectory`) when the path's parent doesn't resolve.
        // Accept either — the contract is that the spawn fails with an
        // io::Error rather than producing a CommandResult.
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ),
            "expected NotFound or NotADirectory, got {err:?}"
        );
    }

    #[test]
    fn near_zero_timeout_kills_immediately() {
        // The polling loop checks `elapsed >= timeout` on every iteration;
        // a sub-poll-interval timeout must still trigger the kill path.
        let (program, args) = long_running_command();
        let arg_refs = args_as_refs(&args);
        let r = run_command(program, &arg_refs, None, &[], Duration::from_millis(10))
            .expect("spawn long cmd");
        assert_eq!(
            r.exit_code, TIMEOUT_EXIT_CODE,
            "expected sentinel on near-zero timeout, got {} (stderr: {:?})",
            r.exit_code, r.stderr
        );
        assert!(
            r.stderr.contains("timed out"),
            "expected stderr to mention timeout, got {:?}",
            r.stderr
        );
    }

    #[test]
    fn stdout_and_stderr_are_separate() {
        // Catches a regression where the pipes were swapped or merged.
        let (program, args) = split_streams_command("gate-out", "gate-err");
        let arg_refs = args_as_refs(&args);
        let r = run_command(program, &arg_refs, None, &[], Duration::from_secs(30))
            .expect("split-streams cmd runs");
        assert_eq!(r.exit_code, 0, "stderr: {}", r.stderr);
        assert!(
            r.stdout.contains("gate-out") && !r.stdout.contains("gate-err"),
            "stdout should hold only the stdout text; got {:?}",
            r.stdout
        );
        assert!(
            r.stderr.contains("gate-err") && !r.stderr.contains("gate-out"),
            "stderr should hold only the stderr text; got {:?}",
            r.stderr
        );
    }

    #[test]
    fn stdin_bytes_reach_child() {
        // Pipe a known payload to a stdin-echoing child and verify the
        // bytes survived the writer-thread + pipe + child-stdout round trip.
        let (program, args) = if cfg!(windows) {
            ("findstr", vec![".".into()])
        } else {
            ("sh", vec!["-c".into(), "cat".into()])
        };
        let arg_refs = args_as_refs(&args);
        let payload = b"gate-stdin-payload
";
        let r = run_command_with_stdin(
            program,
            &arg_refs,
            None,
            &[],
            Duration::from_secs(30),
            payload,
        )
        .expect("stdin-echo command runs");
        assert_eq!(r.exit_code, 0, "stderr: {}", r.stderr);
        assert!(
            r.stdout.contains("gate-stdin-payload"),
            "expected stdin payload in stdout, got {:?}",
            r.stdout
        );
    }

    #[test]
    fn stdin_writer_does_not_break_when_child_ignores_stdin() {
        // `git --version` exits without ever reading stdin, so the writer
        // thread will see EPIPE / ERROR_BROKEN_PIPE while flushing the
        // payload. That error must be swallowed (surfaced via stderr at
        // most) rather than bubble up as `Err`, otherwise a successful
        // child would be reported as a spawn-time I/O failure.
        let payload = b"this should be ignored by the child
";
        let r = run_command_with_stdin(
            "git",
            &["--version"],
            None,
            &[],
            Duration::from_secs(30),
            payload,
        )
        .expect("git --version returns Ok even when stdin is piped and ignored");
        assert_eq!(r.exit_code, 0, "stderr: {}", r.stderr);
        assert!(
            r.stdout.contains("git version"),
            "expected stdout to contain 'git version', got {:?}",
            r.stdout
        );
    }
}
