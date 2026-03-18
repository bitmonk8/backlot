//! Cross-platform integration tests for the `lot` sandboxing library.
//!
//! Run with `--test-threads=1` because some tests modify system state
//! (ACLs on Windows, cgroups on Linux).
//!
//! Diagnostic logging: every test prints to stderr what happened (spawn
//! success/failure, exit status, skip reasons). Run with `--nocapture`
//! to see this output in CI.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

/// Spawn a sandboxed child, panicking on any error.
fn must_spawn(policy: &lot::SandboxPolicy, cmd: &lot::SandboxCommand) -> lot::SandboxedChild {
    let child = lot::spawn(policy, cmd).expect("spawn must succeed");
    eprintln!("[diag] spawn succeeded, pid={}", child.id());
    child
}

/// Platform-appropriate echo command: `cmd /C echo hello` on Windows,
/// `/bin/echo hello` on Unix.
#[cfg(target_os = "windows")]
fn echo_command() -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("cmd"),
        vec!["/C".into(), "echo".into(), "hello".into()],
    )
}

#[cfg(not(target_os = "windows"))]
fn echo_command() -> (PathBuf, Vec<String>) {
    (PathBuf::from("/bin/echo"), vec!["hello".into()])
}

/// Platform-appropriate command that reads a file and prints its contents.
#[cfg(target_os = "windows")]
fn cat_command(path: &std::path::Path) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("cmd"),
        vec![
            "/C".into(),
            "type".into(),
            path.to_string_lossy().into_owned(),
        ],
    )
}

#[cfg(not(target_os = "windows"))]
fn cat_command(path: &std::path::Path) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/cat"),
        vec![path.to_string_lossy().into_owned()],
    )
}

/// Platform-appropriate command that writes to a file.
#[cfg(target_os = "windows")]
fn write_command(path: &std::path::Path) -> (PathBuf, Vec<String>) {
    // `cmd /C echo data > path`
    let target = path.to_string_lossy().into_owned();
    (
        PathBuf::from("cmd"),
        vec!["/C".into(), format!("echo data > \"{target}\"")],
    )
}

#[cfg(not(target_os = "windows"))]
fn write_command(path: &std::path::Path) -> (PathBuf, Vec<String>) {
    let target = path.to_string_lossy().into_owned();
    (
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), format!("echo data > '{target}'")],
    )
}

/// Platform-appropriate command that exits with a given code.
#[cfg(target_os = "windows")]
fn exit_command(code: i32) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("cmd"),
        vec!["/C".into(), format!("exit /B {code}")],
    )
}

#[cfg(not(target_os = "windows"))]
fn exit_command(code: i32) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/sh"),
        vec!["-c".into(), format!("exit {code}")],
    )
}

/// Platform-appropriate command that reads stdin and echoes to stdout.
#[cfg(target_os = "windows")]
fn stdin_echo_command() -> (PathBuf, Vec<String>) {
    // `findstr` with pattern ".*" echoes all stdin lines to stdout.
    (PathBuf::from("findstr"), vec![".*".into()])
}

#[cfg(not(target_os = "windows"))]
fn stdin_echo_command() -> (PathBuf, Vec<String>) {
    (PathBuf::from("/bin/cat"), vec![])
}

/// Build a minimal policy with the given read/write paths.
///
/// On Windows, no `exec_paths` are needed — `AppContainer` can run system
/// executables without explicit grants. On Unix, we add `/bin` (and `/usr/bin`
/// if distinct) so the sandbox can find standard utilities.
fn make_policy(read_paths: Vec<PathBuf>, write_paths: Vec<PathBuf>) -> lot::SandboxPolicy {
    #[allow(unused_mut)]
    let mut exec_paths = Vec::new();

    // Windows: AppContainer inherits access to system binaries; adding
    // System32 to exec_paths would require admin ACL grants.
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/bin").exists() {
            exec_paths.push(PathBuf::from("/bin"));
        }
        if std::path::Path::new("/usr/bin").exists()
            && std::fs::canonicalize("/usr/bin").ok() != std::fs::canonicalize("/bin").ok()
        {
            exec_paths.push(PathBuf::from("/usr/bin"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        exec_paths.push(PathBuf::from("/bin"));
        exec_paths.push(PathBuf::from("/usr/bin"));
    }

    lot::SandboxPolicy::new(
        read_paths,
        write_paths,
        exec_paths,
        Vec::new(),
        false,
        lot::ResourceLimits::default(),
    )
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn test_probe_returns_platform_capabilities() {
    eprintln!("[diag] === test_probe_returns_platform_capabilities ===");
    let caps = lot::probe();
    eprintln!(
        "[diag] probe() = appcontainer={}, namespaces={}, seccomp={}, cgroups_v2={}, seatbelt={}, job_objects={}",
        caps.appcontainer,
        caps.namespaces,
        caps.seccomp,
        caps.cgroups_v2,
        caps.seatbelt,
        caps.job_objects
    );

    #[cfg(target_os = "windows")]
    {
        assert!(caps.appcontainer, "Windows should support AppContainer");
        assert!(caps.job_objects, "Windows should support Job Objects");
        assert!(!caps.namespaces);
        assert!(!caps.seatbelt);
    }

    #[cfg(target_os = "linux")]
    {
        // seccomp should be available on most Linux runners.
        assert!(caps.seccomp, "Linux should support seccomp");
        assert!(!caps.appcontainer);
        assert!(!caps.seatbelt);
    }

    #[cfg(target_os = "macos")]
    {
        assert!(caps.seatbelt, "macOS should support Seatbelt");
        assert!(!caps.appcontainer);
        assert!(!caps.namespaces);
    }
}

#[test]
fn test_spawn_echo() {
    eprintln!("[diag] === test_spawn_echo ===");

    let tmp = TempDir::new().expect("create temp dir");
    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.status.success(),
        "echo should succeed inside sandbox, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "stdout should contain 'hello', got: {stdout:?}"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_spawn_read_allowed_path() {
    eprintln!("[diag] === test_spawn_read_allowed_path ===");

    let tmp = TempDir::new().expect("create temp dir");
    let file_path = tmp.path().join("readable.txt");
    std::fs::write(&file_path, "sandbox_test_data").expect("write test file");

    let (program, args) = cat_command(&file_path);
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.status.success(),
        "cat should succeed for allowed path, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("sandbox_test_data"),
        "should read allowed file, got: {stdout:?}"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_spawn_disallowed_path_blocked() {
    eprintln!("[diag] === test_spawn_disallowed_path_blocked ===");

    // Two separate temp dirs: one allowed, one forbidden.
    let allowed = TempDir::new().expect("create allowed dir");
    let forbidden = TempDir::new().expect("create forbidden dir");
    let secret_file = forbidden.path().join("secret.txt");
    std::fs::write(&secret_file, "secret_data").expect("write secret file");

    let (program, args) = cat_command(&secret_file);
    let policy = make_policy(vec![allowed.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The command must fail because the file is outside the policy.
    assert!(
        !output.status.success(),
        "reading disallowed path should fail, but exited with: {:?}",
        output.status
    );
    // The process must have exited normally (not killed by signal).
    // A signal death means the sandbox killed the process for an unrelated
    // reason (e.g. exec failed), which is a false positive — not a real
    // policy enforcement test.
    assert!(
        output.status.code().is_some(),
        "process should exit normally (not by signal), got: {:?}",
        output.status
    );
    // stdout must NOT contain the secret data.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("secret_data"),
        "sandbox should have blocked reading the secret, but stdout contained it"
    );
    eprintln!(
        "[diag] PASSED: disallowed path blocked (exit code={:?})",
        output.status.code()
    );
}

#[test]
fn test_spawn_write_to_readonly_blocked() {
    eprintln!("[diag] === test_spawn_write_to_readonly_blocked ===");

    let tmp = TempDir::new().expect("create temp dir");
    let target = tmp.path().join("blocked_write.txt");
    let (program, args) = write_command(&target);

    // tmp.path() in read_paths only, not write_paths.
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("[diag] file exists after attempt: {}", target.exists());

    // Writing to a read-only path should fail.
    assert!(
        !output.status.success(),
        "writing to read-only path should fail, but exited with: {:?}",
        output.status
    );
    // Must be a normal exit, not a signal kill (which would be a false positive).
    assert!(
        output.status.code().is_some(),
        "process should exit normally (not by signal), got: {:?}",
        output.status
    );
    // The file must not have been created.
    assert!(
        !target.exists(),
        "file should not exist after blocked write"
    );
    eprintln!(
        "[diag] PASSED: write to read-only path blocked (exit code={:?})",
        output.status.code()
    );
}

#[test]
fn test_cleanup_after_drop() {
    eprintln!("[diag] === test_cleanup_after_drop ===");

    let tmp = TempDir::new().expect("create temp dir");
    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let pid = child.id();
    assert!(pid > 0, "pid should be non-zero");
    eprintln!("[diag] child pid={pid}, dropping now");

    // Drop triggers cleanup (ACL restore on Windows, cgroup removal on Linux).
    drop(child);

    // Platform-specific verification:
    #[cfg(target_os = "windows")]
    {
        // Drop ran restore_from_sentinel for this child's sentinel.
        // We don't call cleanup_stale() here — it scans ALL sentinels
        // globally and would interfere with other tests running in parallel.
        eprintln!("[diag] PASSED: drop completed without panic");
    }

    #[cfg(target_os = "linux")]
    {
        let proc_path = format!("/proc/{pid}");
        let gone = !std::path::Path::new(&proc_path).exists();
        eprintln!("[diag] process gone after drop: {gone}");
        assert!(gone, "process should be gone after drop");
        eprintln!("[diag] PASSED: process cleaned up");
    }

    #[cfg(target_os = "macos")]
    {
        // Check if the process is still alive after drop.
        // signal 0 doesn't send a signal, just checks if pid exists.
        let result = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output();
        let gone = result.map_or(true, |o| !o.status.success());
        eprintln!("[diag] process gone after drop: {gone}");
        eprintln!("[diag] PASSED: cleanup ran (process gone={gone})");
    }
}

#[test]
fn test_spawn_with_piped_stdin() {
    eprintln!("[diag] === test_spawn_with_piped_stdin ===");

    let tmp = TempDir::new().expect("create temp dir");
    let (program, args) = stdin_echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdin(lot::SandboxStdio::Piped);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let mut child = must_spawn(&policy, &cmd);

    // Write to stdin, then close it so the child sees EOF.
    {
        let mut stdin = child.take_stdin().expect("stdin should be piped");
        stdin.write_all(b"piped_input\n").expect("write to stdin");
    }

    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.status.success(),
        "stdin echo should succeed inside sandbox, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("piped_input"),
        "stdout should contain piped input, got: {stdout:?}"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_wait_returns_exit_status() {
    eprintln!("[diag] === test_wait_returns_exit_status ===");

    let tmp = TempDir::new().expect("create temp dir");

    // Test exit code 0.
    {
        eprintln!("[diag] testing exit code 0");
        let (program, args) = exit_command(0);
        let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);
        let mut cmd = lot::SandboxCommand::new(&program);
        cmd.args(&args);
        cmd.stdout(lot::SandboxStdio::Piped);
        cmd.stderr(lot::SandboxStdio::Piped);

        let child = must_spawn(&policy, &cmd);
        let status = child.wait().expect("wait");
        eprintln!(
            "[diag] exit status: {:?}, code: {:?}",
            status,
            status.code()
        );
        assert!(
            status.success(),
            "exit 0 should be success, got: {status:?}"
        );
        eprintln!("[diag] PASSED: exit 0 verified");
    }

    // Test exit code 42.
    {
        eprintln!("[diag] testing exit code 42");
        let (program, args) = exit_command(42);
        let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);
        let mut cmd = lot::SandboxCommand::new(&program);
        cmd.args(&args);
        cmd.stdout(lot::SandboxStdio::Piped);
        cmd.stderr(lot::SandboxStdio::Piped);

        let child = must_spawn(&policy, &cmd);
        let status = child.wait().expect("wait");
        eprintln!(
            "[diag] exit status: {:?}, code: {:?}",
            status,
            status.code()
        );
        assert!(!status.success(), "exit 42 should not be success");
        assert_eq!(
            status.code(),
            Some(42),
            "exit code should be 42, got: {:?}",
            status.code()
        );
        eprintln!("[diag] PASSED: exit 42 verified");
    }
}

#[test]
fn test_deny_path_blocks_access_to_subtree() {
    eprintln!("[diag] === test_deny_path_blocks_access_to_subtree ===");

    let tmp = TempDir::new().expect("create temp dir");
    let allowed_dir = tmp.path().join("workspace");
    let denied_dir = allowed_dir.join("secrets");
    std::fs::create_dir_all(&denied_dir).expect("create denied dir");

    // Write a file inside the denied subtree.
    let secret_file = denied_dir.join("secret.txt");
    std::fs::write(&secret_file, "secret_data").expect("write secret file");

    // Write a file outside the denied subtree (should remain readable).
    let public_file = allowed_dir.join("public.txt");
    std::fs::write(&public_file, "public_data").expect("write public file");

    let (program, args) = cat_command(&secret_file);

    #[allow(unused_mut)]
    let mut exec_paths = Vec::new();
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/bin").exists() {
            exec_paths.push(PathBuf::from("/bin"));
        }
        if std::path::Path::new("/usr/bin").exists()
            && std::fs::canonicalize("/usr/bin").ok() != std::fs::canonicalize("/bin").ok()
        {
            exec_paths.push(PathBuf::from("/usr/bin"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        exec_paths.push(PathBuf::from("/bin"));
        exec_paths.push(PathBuf::from("/usr/bin"));
    }

    let allowed_dir_canon = std::fs::canonicalize(&allowed_dir).expect("canonicalize allowed_dir");
    let denied_dir_canon = std::fs::canonicalize(&denied_dir).expect("canonicalize denied_dir");

    let policy = lot::SandboxPolicy::new(
        vec![allowed_dir_canon],
        vec![],
        exec_paths,
        vec![denied_dir_canon],
        false,
        lot::ResourceLimits::default(),
    );

    // Part 1: reading a file inside the denied subtree must fail.
    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] denied read exit status: {:?}", output.status);
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !output.status.success(),
        "reading denied path should fail, but exited with: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("secret_data"),
        "sandbox should have blocked reading the secret, but stdout contained it"
    );
    eprintln!(
        "[diag] PASSED: deny path blocked access (exit code={:?})",
        output.status.code()
    );

    // Part 2: reading a file outside the denied subtree must still succeed.
    let (pub_program, pub_args) = cat_command(&public_file);
    let mut pub_cmd = lot::SandboxCommand::new(&pub_program);
    pub_cmd.args(&pub_args);
    pub_cmd.stdout(lot::SandboxStdio::Piped);
    pub_cmd.stderr(lot::SandboxStdio::Piped);

    let pub_child = must_spawn(&policy, &pub_cmd);
    let pub_output = pub_child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] public read exit status: {:?}", pub_output.status);

    assert!(
        pub_output.status.success(),
        "reading non-denied path should succeed, but exited with: {:?}\nstderr: {}",
        pub_output.status,
        String::from_utf8_lossy(&pub_output.stderr)
    );
    let pub_stdout = String::from_utf8_lossy(&pub_output.stdout);
    assert!(
        pub_stdout.contains("public_data"),
        "non-denied file should be readable, but stdout was: {pub_stdout}"
    );
    eprintln!("[diag] PASSED: non-denied sibling remains accessible");
}

// ── Tokio timeout tests ────────────────────────────────────────────

/// Platform-appropriate long-running command (sleep substitute).
#[cfg(feature = "tokio")]
fn sleep_command(seconds: u32) -> (PathBuf, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        // ping -n <seconds+1> 127.0.0.1 as a sleep substitute
        (
            PathBuf::from("ping"),
            vec!["-n".into(), (seconds + 1).to_string(), "127.0.0.1".into()],
        )
    }

    #[cfg(not(target_os = "windows"))]
    {
        (PathBuf::from("/bin/sleep"), vec![seconds.to_string()])
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_wait_with_output_timeout_completes_before_timeout() {
    eprintln!("[diag] === test_wait_with_output_timeout_completes_before_timeout ===");

    let tmp = TempDir::new().expect("create temp dir");
    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);

    let result = child
        .wait_with_output_timeout(std::time::Duration::from_secs(10))
        .await;

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("hello"),
                "expected 'hello' in output, got: {stdout}"
            );
            eprintln!("[diag] PASSED: completed before timeout");
        }
        Err(e) => panic!("expected success, got: {e:?}"),
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_wait_with_output_timeout_kills_on_timeout() {
    eprintln!("[diag] === test_wait_with_output_timeout_kills_on_timeout ===");

    let tmp = TempDir::new().expect("create temp dir");
    let (program, args) = sleep_command(60);

    #[cfg(target_os = "windows")]
    let policy = {
        let p = make_policy(vec![tmp.path().to_path_buf()], vec![]);
        // Rebuild with network enabled for ping
        lot::SandboxPolicy::new(
            p.read_paths().to_vec(),
            p.write_paths().to_vec(),
            p.exec_paths().to_vec(),
            p.deny_paths().to_vec(),
            true,
            p.limits().clone(),
        )
    };
    #[cfg(not(target_os = "windows"))]
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![]);

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);

    let result = child
        .wait_with_output_timeout(std::time::Duration::from_millis(200))
        .await;

    match result {
        Err(lot::SandboxError::Timeout(d)) => {
            assert!(
                d.as_millis() >= 200,
                "timeout duration should match requested"
            );
            eprintln!("[diag] PASSED: timeout fired and child killed");
        }
        other => panic!("expected Timeout error, got: {other:?}"),
    }
}
