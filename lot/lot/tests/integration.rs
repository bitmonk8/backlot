//! Cross-platform integration tests for the `lot` sandboxing library.
//!
//! Tests are parallel-safe by design: unique profile names, unique temp
//! dirs, idempotent ACE grants. Tests that call `cleanup_stale()` use
//! per-test sentinel directories to avoid shared state.
//!
//! Diagnostic logging: every test prints to stderr what happened (spawn
//! success/failure, exit status, skip reasons). Run with `--nocapture`
//! to see this output in CI.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::path::PathBuf;

use common::{
    make_sandbox_cmd, make_temp_dir, memory_hog_command, network_connect_command,
    platform_exec_paths, set_sandbox_env, sleep_command,
};

/// Spawn a sandboxed child, panicking on any error including missing prerequisites.
fn must_spawn(policy: &lot::SandboxPolicy, cmd: &lot::SandboxCommand) -> lot::SandboxedChild {
    match lot::spawn(policy, cmd) {
        Ok(child) => {
            eprintln!("[diag] spawn succeeded, pid={}", child.id());
            child
        }
        Err(e) => panic!("spawn must succeed: {e}"),
    }
}

/// Log diagnostic output from a sandboxed child for CI debugging.
fn log_output(output: &std::process::Output) {
    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    // cmd.exe /C does NOT follow CommandLineToArgvW escaping rules —
    // inner quotes get mangled by build_command_line's correct escaping.
    // Use unquoted path; CI temp paths don't contain spaces.
    let target = path.to_string_lossy().into_owned();
    (
        PathBuf::from("cmd"),
        vec!["/C".into(), format!("echo data > {target}")],
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
fn make_policy(
    read_paths: impl AsRef<[PathBuf]>,
    write_paths: impl AsRef<[PathBuf]>,
    scratch: &std::path::Path,
) -> lot::SandboxPolicy {
    make_policy_with_sentinel_dir(read_paths.as_ref(), write_paths.as_ref(), scratch, None)
}

fn make_policy_with_sentinel_dir(
    read_paths: &[PathBuf],
    write_paths: &[PathBuf],
    scratch: &std::path::Path,
    sentinel_dir: Option<&std::path::Path>,
) -> lot::SandboxPolicy {
    let mut builder = lot::SandboxPolicyBuilder::new();
    for p in read_paths {
        builder = builder.read_path(p).expect("read_path");
    }
    builder = builder.write_path(scratch).expect("write_path scratch");
    for p in write_paths {
        builder = builder.write_path(p).expect("write_path");
    }
    for p in &platform_exec_paths() {
        builder = builder.exec_path(p).expect("exec_path");
    }
    if let Some(dir) = sentinel_dir {
        builder = builder.sentinel_dir(dir);
    }
    builder.build().expect("build policy")
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

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    log_output(&output);

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

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let file_path = tmp.path().join("readable.txt");
    std::fs::write(&file_path, "sandbox_test_data").expect("write test file");

    let (program, args) = cat_command(&file_path);
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    log_output(&output);

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

/// Grant read access to a single file (not its parent directory) and verify
/// the sandboxed process can read it. Exercises the file bind-mount path in
/// `create_mount_target` on Linux. Unix-only: Windows AppContainer requires
/// parent directory access for file reads.
#[cfg(unix)]
#[test]
fn test_spawn_read_single_file() {
    eprintln!("[diag] === test_spawn_read_single_file ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let file_path = tmp.path().join("single_file.txt");
    std::fs::write(&file_path, "file_mount_test").expect("write test file");

    let file_canon = std::fs::canonicalize(&file_path).expect("canonicalize file");

    let (program, args) = cat_command(&file_canon);

    // Grant the single file, not the directory
    let policy = lot::SandboxPolicy::new(
        vec![file_canon],
        vec![scratch.path().to_path_buf()],
        platform_exec_paths(),
        Vec::new(),
        false,
        lot::ResourceLimits::default(),
    );

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    log_output(&output);

    assert!(
        output.status.success(),
        "cat should succeed for single file read path, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("file_mount_test"),
        "should read single-file mount, got: {stdout:?}"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_spawn_disallowed_path_blocked() {
    eprintln!("[diag] === test_spawn_disallowed_path_blocked ===");

    // Two separate temp dirs: one allowed, one forbidden.
    let allowed = make_temp_dir();
    let forbidden = make_temp_dir();
    let scratch = make_temp_dir();

    let secret_file = forbidden.path().join("secret.txt");
    std::fs::write(&secret_file, "secret_data").expect("write secret file");

    let (program, args) = cat_command(&secret_file);
    let policy = make_policy(vec![allowed.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    log_output(&output);

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

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let target = tmp.path().join("blocked_write.txt");
    let (program, args) = write_command(&target);

    // tmp.path() in read_paths only, not write_paths.
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    log_output(&output);
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

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();
    let sentinel_dir = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy_with_sentinel_dir(
        &[tmp.path().to_path_buf()],
        &[],
        scratch.path(),
        Some(sentinel_dir.path()),
    );

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

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
        // Drop ran restore_acls_and_delete_sentinel. Verify no stale sentinel remains
        // for this child by checking cleanup_stale succeeds without error.
        let cleanup_result = lot::cleanup_stale(Some(sentinel_dir.path()));
        assert!(
            cleanup_result.is_ok(),
            "cleanup_stale after drop should succeed: {:?}",
            cleanup_result.err()
        );
        eprintln!("[diag] PASSED: drop completed and cleanup_stale OK");
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
        // signal 0 checks if pid exists without sending a signal.
        let result = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output();
        let gone = result.map_or(true, |o| !o.status.success());
        eprintln!("[diag] process gone after drop: {gone}");
        assert!(gone, "process should be gone after drop");
        eprintln!("[diag] PASSED: process cleaned up");
    }
}

#[test]
fn test_spawn_with_piped_stdin() {
    eprintln!("[diag] === test_spawn_with_piped_stdin ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = stdin_echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

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

    log_output(&output);

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

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    // Test exit code 0.
    {
        eprintln!("[diag] testing exit code 0");
        let (program, args) = exit_command(0);
        let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());
        let mut cmd = lot::SandboxCommand::new(&program);
        set_sandbox_env(&mut cmd, scratch.path());

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
        let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());
        let mut cmd = lot::SandboxCommand::new(&program);
        set_sandbox_env(&mut cmd, scratch.path());

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

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

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

    let allowed_dir_canon = std::fs::canonicalize(&allowed_dir).expect("canonicalize allowed_dir");
    let denied_dir_canon = std::fs::canonicalize(&denied_dir).expect("canonicalize denied_dir");

    let policy = lot::SandboxPolicy::new(
        vec![allowed_dir_canon],
        vec![scratch.path().to_path_buf()],
        platform_exec_paths(),
        vec![denied_dir_canon],
        false,
        lot::ResourceLimits::default(),
    );

    // Part 1: reading a file inside the denied subtree must fail.
    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

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
    set_sandbox_env(&mut pub_cmd, scratch.path());

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

/// Build a policy with a denied subdirectory inside a granted parent.
fn make_deny_policy(
    parent: &std::path::Path,
    denied: &std::path::Path,
    write: bool,
    scratch: &std::path::Path,
) -> lot::SandboxPolicy {
    let parent_canon = std::fs::canonicalize(parent).expect("canonicalize parent");
    let denied_canon = std::fs::canonicalize(denied).expect("canonicalize denied");

    let (read, mut write_paths) = if write {
        (vec![], vec![parent_canon])
    } else {
        (vec![parent_canon], vec![])
    };
    write_paths.push(scratch.to_path_buf());

    lot::SandboxPolicy::new(
        read,
        write_paths,
        platform_exec_paths(),
        vec![denied_canon],
        false,
        lot::ResourceLimits::default(),
    )
}

#[test]
fn test_deny_path_blocks_write() {
    eprintln!("[diag] === test_deny_path_blocks_write ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let parent = tmp.path().join("workspace");
    let denied = parent.join("readonly_zone");
    std::fs::create_dir_all(&denied).expect("create denied dir");

    let target_in_denied = denied.join("blocked.txt");
    let target_in_parent = parent.join("allowed.txt");

    let policy = make_deny_policy(&parent, &denied, true, scratch.path());

    // Attempt write inside denied subdirectory — should fail.
    let (program, args) = write_command(&target_in_denied);
    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait");

    eprintln!("[diag] denied write exit: {:?}", output.status);
    assert!(
        !target_in_denied.exists(),
        "file in denied path must never be created, regardless of exit code"
    );

    // Attempt write to parent (outside denied) — should succeed.
    let (program2, args2) = write_command(&target_in_parent);
    let mut cmd2 = lot::SandboxCommand::new(&program2);
    set_sandbox_env(&mut cmd2, scratch.path());

    cmd2.args(&args2);
    cmd2.stdout(lot::SandboxStdio::Piped);
    cmd2.stderr(lot::SandboxStdio::Piped);

    let child2 = must_spawn(&policy, &cmd2);
    let output2 = child2.wait_with_output().expect("wait");

    eprintln!("[diag] allowed write exit: {:?}", output2.status);
    assert!(
        output2.status.success(),
        "write to parent (non-denied) should succeed, got: {:?}\nstderr: {}",
        output2.status,
        String::from_utf8_lossy(&output2.stderr)
    );
    assert!(
        target_in_parent.exists(),
        "file should exist in allowed parent"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_deny_path_blocks_execution() {
    eprintln!("[diag] === test_deny_path_blocks_execution ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let parent = tmp.path().join("workspace");
    let denied = parent.join("no_exec");
    std::fs::create_dir_all(&denied).expect("create denied dir");

    // Place a script/executable in the denied path.
    #[cfg(target_os = "windows")]
    {
        let script = denied.join("test.bat");
        std::fs::write(&script, "@echo SHOULD_NOT_RUN\r\n").expect("write bat");

        let policy = make_deny_policy(&parent, &denied, false, scratch.path());

        let mut cmd = lot::SandboxCommand::new("cmd.exe");
        set_sandbox_env(&mut cmd, scratch.path());

        cmd.args(["/C", &script.to_string_lossy()]);
        cmd.stdout(lot::SandboxStdio::Piped);
        cmd.stderr(lot::SandboxStdio::Piped);

        let child = must_spawn(&policy, &cmd);
        let output = child.wait_with_output().expect("wait");

        assert!(
            !output.status.success(),
            "process should have failed when executing from denied path"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("SHOULD_NOT_RUN"),
            "execution inside denied path should be blocked, got: {stdout}"
        );
        eprintln!("[diag] PASSED");
    }

    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;

        let script = denied.join("test.sh");
        std::fs::write(&script, "#!/bin/sh\necho SHOULD_NOT_RUN\n").expect("write script");

        // Make executable
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let policy = make_deny_policy(&parent, &denied, false, scratch.path());

        let mut cmd = lot::SandboxCommand::new("/bin/sh");
        set_sandbox_env(&mut cmd, scratch.path());

        cmd.args(["-c", &format!("{}", script.display())]);
        cmd.stdout(lot::SandboxStdio::Piped);
        cmd.stderr(lot::SandboxStdio::Piped);

        let child = must_spawn(&policy, &cmd);
        let output = child.wait_with_output().expect("wait");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("SHOULD_NOT_RUN"),
            "execution inside denied path should be blocked, got: {stdout}"
        );
        eprintln!("[diag] PASSED");
    }
}

#[cfg(unix)]
#[test]
fn test_symlink_into_deny_path() {
    eprintln!("[diag] === test_symlink_into_deny_path ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let parent = tmp.path().join("workspace");
    let denied = parent.join("forbidden");
    std::fs::create_dir_all(&denied).expect("create denied dir");

    let secret_file = denied.join("secret.txt");
    std::fs::write(&secret_file, "secret_via_symlink").expect("write secret");

    // Create symlink in the allowed area pointing into the denied area.
    let symlink_path = parent.join("sneaky_link.txt");

    std::os::unix::fs::symlink(&secret_file, &symlink_path).expect("create symlink");

    let policy = make_deny_policy(&parent, &denied, false, scratch.path());

    let (program, args) = cat_command(&symlink_path);
    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait");

    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("[diag] exit: {:?}, stdout: {stdout:?}", output.status);

    // The symlink target is in the denied area. Reading should fail or
    // at minimum not return the secret data.
    assert!(
        !stdout.contains("secret_via_symlink"),
        "symlink into denied path should not expose secret data"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_double_wait_behavior() {
    eprintln!("[diag] === test_double_wait_behavior ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = exit_command(0);
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    set_sandbox_env(&mut cmd, scratch.path());

    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);

    let status = child.wait().expect("first wait should succeed");
    eprintln!("[diag] first wait: {status:?}");

    // Unix: second wait returns InvalidInput (pid already reaped).
    // Windows: second wait succeeds (WaitForSingleObject on exited handle is idempotent).
    #[cfg(unix)]
    {
        let err = child.wait().expect_err("second wait should fail on Unix");
        eprintln!("[diag] second wait error: {err}");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidInput,
            "expected InvalidInput, got: {:?}",
            err.kind()
        );
    }
    #[cfg(windows)]
    {
        let status2 = child.wait().expect("second wait should succeed on Windows");
        eprintln!("[diag] second wait: {status2:?}");
        assert_eq!(
            status.code(),
            status2.code(),
            "double wait should return same exit code"
        );
    }
    eprintln!("[diag] PASSED");
}

#[test]
#[cfg(unix)]
fn test_unix_tmpdir_in_write_paths_succeeds() {
    eprintln!("[diag] === test_unix_tmpdir_in_write_paths_succeeds ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    // scratch is already a write_path via make_policy, so TMPDIR is valid.
    cmd.env("TMPDIR", scratch.path());
    cmd.env("PATH", "/usr/bin:/bin");
    cmd.args(&args);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "process should succeed");
    eprintln!("[diag] PASSED");
}

#[test]
#[cfg(unix)]
fn test_unix_tmpdir_outside_write_paths_returns_invalid_policy() {
    eprintln!("[diag] === test_unix_tmpdir_outside_write_paths_returns_invalid_policy ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();
    let outside = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    cmd.env("TMPDIR", outside.path());
    cmd.env("PATH", "/usr/bin:/bin");
    cmd.args(&args);

    match lot::spawn(&policy, &cmd) {
        Err(lot::SandboxError::InvalidPolicy(msg)) => {
            assert!(msg.contains("TMPDIR"), "error should mention TMPDIR: {msg}");
            eprintln!("[diag] PASSED: InvalidPolicy for inaccessible TMPDIR");
        }
        other => panic!("expected InvalidPolicy for inaccessible TMPDIR, got: {other:?}"),
    }
}

#[test]
#[cfg(target_os = "windows")]
fn test_windows_temp_in_write_paths_succeeds() {
    eprintln!("[diag] === test_windows_temp_in_write_paths_succeeds ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
    cmd.env("PATH", format!(r"{sys_root}\System32"));
    cmd.env("TMPDIR", scratch.path());
    cmd.env("TEMP", scratch.path());
    cmd.env("TMP", scratch.path());
    cmd.forward_common_env();
    cmd.args(&args);
    cmd.stdout(lot::SandboxStdio::Piped);
    cmd.stderr(lot::SandboxStdio::Piped);

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "process should succeed");
    eprintln!("[diag] PASSED");
}

#[test]
#[cfg(target_os = "windows")]
fn test_windows_temp_outside_write_paths_returns_invalid_policy() {
    eprintln!("[diag] === test_windows_temp_outside_write_paths_returns_invalid_policy ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();
    let outside = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let mut cmd = lot::SandboxCommand::new(&program);
    let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
    cmd.env("PATH", format!(r"{sys_root}\System32"));
    cmd.env("TMPDIR", outside.path());
    cmd.env("TEMP", outside.path());
    cmd.env("TMP", outside.path());
    cmd.forward_common_env();
    cmd.args(&args);

    match lot::spawn(&policy, &cmd) {
        Err(lot::SandboxError::InvalidPolicy(msg)) => {
            assert!(msg.contains("TEMP"), "error should mention TEMP: {msg}");
            eprintln!("[diag] PASSED: InvalidPolicy for inaccessible TEMP");
        }
        other => panic!("expected InvalidPolicy for inaccessible TEMP, got: {other:?}"),
    }
}

// ── Child lifecycle, I/O handle, and policy enforcement tests ───────

#[test]
fn test_try_wait_returns_none_then_some() {
    eprintln!("[diag] === test_try_wait_returns_none_then_some ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = sleep_command(60);
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);

    // Process just started — should not have exited yet.
    let poll = child.try_wait().expect("try_wait");
    eprintln!("[diag] first try_wait: {poll:?}");
    assert!(poll.is_none(), "process should still be running");

    // Kill, then poll until reaped -- bounded to prevent CI hang.
    child.kill().expect("kill");
    let mut reaped = false;
    for _ in 0..200 {
        match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!("[diag] try_wait after kill: {status:?}");
                assert!(!status.success(), "killed process should not succeed");
                reaped = true;
                break;
            }
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("try_wait failed: {e}"),
        }
    }
    assert!(reaped, "process was not reaped within 10s after kill");
    eprintln!("[diag] PASSED");
}

#[test]
fn test_kill_terminates_running_process() {
    eprintln!("[diag] === test_kill_terminates_running_process ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = sleep_command(60);
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);

    child.kill().expect("kill should succeed");
    let status = child.wait().expect("wait after kill");
    eprintln!("[diag] exit status after kill: {status:?}");
    assert!(
        !status.success(),
        "killed process should not report success"
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_kill_and_cleanup() {
    eprintln!("[diag] === test_kill_and_cleanup ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();
    let sentinel_dir = make_temp_dir();

    let (program, args) = sleep_command(60);
    let policy = make_policy_with_sentinel_dir(
        &[tmp.path().to_path_buf()],
        &[],
        scratch.path(),
        Some(sentinel_dir.path()),
    );
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);
    let pid = child.id();

    child
        .kill_and_cleanup()
        .expect("kill_and_cleanup should succeed");

    // Verify the process is actually gone.
    #[cfg(target_os = "linux")]
    {
        let proc_path = format!("/proc/{pid}");
        assert!(
            !std::path::Path::new(&proc_path).exists(),
            "process should be gone after kill_and_cleanup"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let result = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output();
        let gone = result.map_or(true, |o| !o.status.success());
        assert!(gone, "process should be gone after kill_and_cleanup");
    }
    #[cfg(target_os = "windows")]
    {
        // On Windows, kill_and_cleanup closes the job handle which kills
        // all processes. Verify cleanup_stale succeeds.
        let cleanup_result = lot::cleanup_stale(Some(sentinel_dir.path()));
        assert!(
            cleanup_result.is_ok(),
            "cleanup_stale after kill_and_cleanup should succeed: {:?}",
            cleanup_result.err()
        );
    }
    eprintln!("[diag] PASSED (pid={pid} cleaned up)");
}

#[test]
fn test_take_stdout_and_stderr() {
    eprintln!("[diag] === test_take_stdout_and_stderr ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = echo_command();
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let mut child = must_spawn(&policy, &cmd);

    let mut stdout_handle = child.take_stdout().expect("take_stdout should return Some");
    let mut stderr_handle = child.take_stderr().expect("take_stderr should return Some");

    // Second take returns None (already taken).
    assert!(
        child.take_stdout().is_none(),
        "second take_stdout should be None"
    );
    assert!(
        child.take_stderr().is_none(),
        "second take_stderr should be None"
    );

    let status = child.wait().expect("wait");
    eprintln!("[diag] exit status: {status:?}");

    let mut stdout_buf = String::new();
    stdout_handle
        .read_to_string(&mut stdout_buf)
        .expect("read stdout");
    eprintln!("[diag] stdout from taken handle: {stdout_buf:?}");
    assert!(
        stdout_buf.contains("hello"),
        "taken stdout should contain 'hello', got: {stdout_buf:?}"
    );

    let mut stderr_buf = String::new();
    stderr_handle
        .read_to_string(&mut stderr_buf)
        .expect("read stderr");
    eprintln!("[diag] stderr from taken handle: {stderr_buf:?}");
    // stderr may be empty for echo, just verify read succeeded.

    assert!(status.success(), "echo should succeed, got: {status:?}");
    eprintln!("[diag] PASSED");
}

#[test]
fn test_sandbox_policy_builder_basic() {
    eprintln!("[diag] === test_sandbox_policy_builder_basic ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let mut builder = lot::SandboxPolicyBuilder::new()
        .read_path(tmp.path())
        .expect("read_path")
        .write_path(scratch.path())
        .expect("write_path")
        .allow_network(false);

    // Add exec paths individually so the builder can canonicalize and deduplicate.
    for p in platform_exec_paths() {
        builder = builder.exec_path(p).expect("exec_path");
    }

    let policy = builder
        .build()
        .expect("builder should produce valid policy");

    let (program, args) = echo_command();
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");
    eprintln!("[diag] exit status: {:?}", output.status);

    assert!(
        output.status.success(),
        "builder-spawned process should succeed, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "stdout should contain 'hello', got: {stdout:?}"
    );
    eprintln!("[diag] PASSED");
}

// macOS RLIMIT_AS enforcement is unreliable — setrlimit(RLIMIT_AS) returns
// EINVAL when the requested limit is below current virtual memory usage,
// which is common in test environments.
#[test]
fn test_memory_limit_enforcement() {
    eprintln!("[diag] === test_memory_limit_enforcement ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = memory_hog_command();

    // 512 MB limit — below the 1 GB allocation the child attempts.
    let policy = lot::SandboxPolicy::new(
        vec![tmp.path().to_path_buf()],
        vec![scratch.path().to_path_buf()],
        platform_exec_paths(),
        Vec::new(),
        false,
        lot::ResourceLimits {
            max_memory_bytes: Some(512 * 1024 * 1024),
            ..lot::ResourceLimits::default()
        },
    );

    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    // macOS RLIMIT_AS can fail with EINVAL when the limit is below
    // current virtual memory usage. Skip rather than false-pass.
    let child = match lot::spawn(&policy, &cmd) {
        Ok(c) => {
            eprintln!("[diag] spawn succeeded, pid={}", c.id());
            c
        }
        Err(lot::SandboxError::Setup(ref msg))
            if cfg!(target_os = "macos") && msg.contains("setrlimit") =>
        {
            eprintln!("[diag] SKIPPED: test_memory_limit_enforcement — {msg}");
            println!("[diag] SKIPPED: test_memory_limit_enforcement — {msg}");
            return;
        }
        Err(e) => panic!("spawn must succeed: {e}"),
    };
    let output = child.wait_with_output().expect("wait_with_output");
    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The child should fail or be killed due to memory limit.
    assert!(
        !output.status.success(),
        "process exceeding memory limit should fail, got: {:?}",
        output.status
    );
    eprintln!("[diag] PASSED");
}

#[test]
fn test_allow_network_false_blocks_connections() {
    eprintln!("[diag] === test_allow_network_false_blocks_connections ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let (program, args) = network_connect_command();

    let policy = lot::SandboxPolicy::new(
        vec![tmp.path().to_path_buf()],
        vec![scratch.path().to_path_buf()],
        platform_exec_paths(),
        Vec::new(),
        false, // network denied
        lot::ResourceLimits::default(),
    );

    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");
    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Network should be blocked by the sandbox.
    assert!(
        !output.status.success(),
        "network connection should fail when allow_network=false, got: {:?}",
        output.status
    );
    eprintln!("[diag] PASSED");
}

// ── Windows symlink deny-path test ─────────────────────────────────

// Requires Developer Mode or elevation for symlink creation.
// Ignored by default; run with `--include-ignored` when Developer Mode is enabled.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires Developer Mode or elevation for symlink creation"]
fn test_symlink_into_deny_path_windows() {
    eprintln!("[diag] === test_symlink_into_deny_path_windows ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let parent = tmp.path().join("workspace");
    let denied = parent.join("forbidden");
    std::fs::create_dir_all(&denied).expect("create denied dir");

    let secret_file = denied.join("secret.txt");
    std::fs::write(&secret_file, "secret_via_symlink").expect("write secret");

    let symlink_path = parent.join("sneaky_link.txt");

    // Symlink creation requires Developer Mode or elevation. If unavailable,
    // fail explicitly so CI does not silently report the test as passed.
    std::os::windows::fs::symlink_file(&secret_file, &symlink_path)
        .expect("symlink creation failed — enable Developer Mode or run elevated");

    let policy = make_deny_policy(&parent, &denied, false, scratch.path());

    let (program, args) = cat_command(&symlink_path);
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait");

    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("[diag] exit: {:?}, stdout: {stdout:?}", output.status);

    assert!(
        !output.status.success(),
        "reading via symlink into denied path should fail, got: {:?}",
        output.status
    );
    assert!(
        !stdout.contains("secret_via_symlink"),
        "symlink into denied path should not expose secret data"
    );
    eprintln!("[diag] PASSED");
}

// ── Integration tests for write-to-allowed-path ────────

#[test]
fn test_write_to_allowed_path_succeeds() {
    eprintln!("[diag] === test_write_to_allowed_path_succeeds ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let target = scratch.path().join("allowed_write.txt");
    let (program, args) = write_command(&target);

    // scratch is in write_paths via make_policy.
    let policy = make_policy(vec![tmp.path().to_path_buf()], vec![], scratch.path());

    let cmd = make_sandbox_cmd(&program, &args, scratch.path());
    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    eprintln!(
        "[diag] stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        output.status.success(),
        "write to allowed path should succeed, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        target.exists(),
        "file should exist after writing to allowed path"
    );
    eprintln!("[diag] PASSED");
}

// ── Builder-based Windows integration test ──────────────

#[test]
#[cfg(target_os = "windows")]
fn test_windows_builder_based_spawn() {
    eprintln!("[diag] === test_windows_builder_based_spawn ===");

    let tmp = make_temp_dir();
    let scratch = make_temp_dir();

    let policy = lot::SandboxPolicyBuilder::new()
        .read_path(tmp.path())
        .expect("read_path")
        .write_path(scratch.path())
        .expect("write_path")
        .allow_network(false)
        .build()
        .expect("build policy via builder");

    let (program, args) = echo_command();
    let cmd = make_sandbox_cmd(&program, &args, scratch.path());

    let child = must_spawn(&policy, &cmd);
    let output = child.wait_with_output().expect("wait_with_output");

    eprintln!("[diag] exit status: {:?}", output.status);
    assert!(
        output.status.success(),
        "builder-based Windows spawn should succeed: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "stdout should contain 'hello', got: {stdout:?}"
    );
    eprintln!("[diag] PASSED");
}

// ── Tokio timeout tests ──────────────────────────────────────────────

#[cfg(feature = "tokio")]
mod tokio_tests {
    #[cfg(target_os = "windows")]
    use super::common::{make_temp_dir, set_sandbox_env};

    fn spawn_sleep(seconds: u32) -> (lot::SandboxedChild, Vec<tempfile::TempDir>) {
        #[cfg(unix)]
        {
            let policy = lot::SandboxPolicyBuilder::new()
                .read_path("/usr")
                .expect("read_path /usr")
                .read_path("/bin")
                .expect("read_path /bin")
                .build()
                .expect("build policy");
            let mut cmd = lot::SandboxCommand::new("/bin/sleep");
            cmd.arg(seconds.to_string());
            cmd.stdout(lot::SandboxStdio::Piped);
            cmd.stderr(lot::SandboxStdio::Piped);
            (
                lot::spawn(&policy, &cmd).expect("spawn_sleep must succeed"),
                vec![],
            )
        }

        #[cfg(windows)]
        {
            let tmp = make_temp_dir();
            let scratch = make_temp_dir();
            let policy = lot::SandboxPolicy::new(
                vec![tmp.path().to_path_buf()],
                vec![scratch.path().to_path_buf()],
                vec![],
                vec![],
                false,
                lot::ResourceLimits::default(),
            );
            let mut cmd = lot::SandboxCommand::new("powershell");
            cmd.args(["-Command", &format!("Start-Sleep -Seconds {seconds}")]);
            cmd.stdout(lot::SandboxStdio::Piped);
            cmd.stderr(lot::SandboxStdio::Piped);
            set_sandbox_env(&mut cmd, scratch.path());
            (
                lot::spawn(&policy, &cmd).expect("spawn_sleep must succeed"),
                vec![tmp, scratch],
            )
        }
    }

    #[tokio::test]
    async fn timeout_fires_on_long_running_child() {
        let (child, _temps) = spawn_sleep(60);

        let result = child
            .wait_with_output_timeout(std::time::Duration::from_millis(200))
            .await;

        match result {
            Err(lot::SandboxError::Timeout(d)) => {
                assert!(
                    d.as_millis() >= 200,
                    "timeout duration should match requested"
                );
            }
            other => panic!("expected Timeout error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn zero_timeout_returns_timeout_error() {
        let (child, _temps) = spawn_sleep(60);

        let result = child
            .wait_with_output_timeout(std::time::Duration::ZERO)
            .await;

        match result {
            Err(lot::SandboxError::Timeout(d)) => {
                assert_eq!(d, std::time::Duration::ZERO);
            }
            other => panic!("expected Timeout error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fast_child_completes_before_timeout() {
        #[cfg(unix)]
        {
            let policy = lot::SandboxPolicyBuilder::new()
                .read_path("/usr")
                .expect("read_path /usr")
                .read_path("/bin")
                .expect("read_path /bin")
                .build()
                .expect("build policy");
            let mut cmd = lot::SandboxCommand::new("/bin/echo");
            cmd.arg("hello");
            cmd.stdout(lot::SandboxStdio::Piped);
            cmd.stderr(lot::SandboxStdio::Piped);

            let child = lot::spawn(&policy, &cmd).expect("spawn must succeed");

            let result = child
                .wait_with_output_timeout(std::time::Duration::from_secs(10))
                .await;

            match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert_eq!(stdout.trim(), "hello");
                }
                Err(e) => panic!("expected success, got: {e:?}"),
            }
        }

        #[cfg(windows)]
        {
            let tmp = make_temp_dir();
            let scratch = make_temp_dir();
            let policy = lot::SandboxPolicy::new(
                vec![tmp.path().to_path_buf()],
                vec![scratch.path().to_path_buf()],
                vec![],
                vec![],
                false,
                lot::ResourceLimits::default(),
            );
            let mut cmd = lot::SandboxCommand::new("cmd.exe");
            cmd.args(["/C", "echo hello"]);
            cmd.stdout(lot::SandboxStdio::Piped);
            cmd.stderr(lot::SandboxStdio::Piped);
            set_sandbox_env(&mut cmd, scratch.path());

            let child = lot::spawn(&policy, &cmd).expect("spawn must succeed");

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
                }
                Err(e) => panic!("expected success, got: {e:?}"),
            }
        }
    }
}
