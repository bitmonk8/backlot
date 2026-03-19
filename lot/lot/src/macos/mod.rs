#![allow(unsafe_code)]

pub mod seatbelt;

use std::io;
use std::sync::atomic::AtomicBool;

use crate::command::SandboxCommand;
use crate::policy::SandboxPolicy;
use crate::unix;
use crate::unix::{KillStyle, UnixSandboxedChild};
use crate::{PlatformCapabilities, Result, SandboxError, SandboxedChild};

pub const fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: seatbelt::available(),
        appcontainer: false,
        job_objects: false,
    }
}

/// Apply resource limits via setrlimit.
///
/// Limitation: RLIMIT_AS is a soft limit — processes can bypass it with
/// MAP_NORESERVE (lazy-commit mappings that don't count against the address
/// space limit). macOS has no kernel-enforced memory cgroup equivalent.
///
/// # Safety
/// Must only be called from the forked child before exec.
unsafe fn apply_resource_limits(policy: &SandboxPolicy) -> io::Result<()> {
    if let Some(max_mem) = policy.limits().max_memory_bytes {
        let rlim = libc::rlimit {
            rlim_cur: max_mem,
            rlim_max: max_mem,
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &raw const rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if let Some(max_procs) = policy.limits().max_processes {
        let rlim = libc::rlimit {
            rlim_cur: u64::from(max_procs),
            rlim_max: u64::from(max_procs),
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &raw const rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if let Some(max_cpu) = policy.limits().max_cpu_seconds {
        let rlim = libc::rlimit {
            rlim_cur: max_cpu,
            rlim_max: max_cpu,
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &raw const rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    let program_path = std::path::Path::new(&command.program);
    // Resolve symlinks/firmlinks so the seatbelt profile matches the real path.
    // On macOS 13+, /bin and /usr/bin are firmlinks to /System/Cryptexes/OS/...
    let resolved =
        std::fs::canonicalize(program_path).unwrap_or_else(|_| program_path.to_path_buf());
    let profile = seatbelt::generate_profile(policy, &resolved);

    let prefork = unix::prepare_prefork(command)
        .map_err(|e| SandboxError::Setup(format!("pre-fork preparation: {e}")))?;

    // Set up stdio pipes before forking
    let unix::StdioPipes {
        child_stdin,
        child_stdout,
        child_stderr,
        parent_stdin,
        parent_stdout,
        parent_stderr,
    } = unix::setup_stdio_pipes(command)
        .map_err(|e| SandboxError::Setup(format!("stdio pipe setup: {e}")))?;

    // Error pipe: child writes errno here if anything fails before exec
    let (err_pipe_rd, err_pipe_wr) =
        unix::make_pipe().map_err(|e| SandboxError::Setup(format!("error pipe: {e}")))?;

    // SAFETY: We fork a child process. Between fork and _exit/exec in the child,
    // we call sandbox_init (which Apple's own code calls post-fork) and
    // async-signal-safe functions. The child is single-threaded after fork.
    let child_pid = unsafe { libc::fork() };

    if child_pid < 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }

    if child_pid == 0 {
        // === CHILD PROCESS (single-threaded after fork) ===

        // Step constants for error reporting via child_bail protocol
        const STEP_SETSID: i32 = 1;
        const STEP_SEATBELT: i32 = 2;
        const STEP_RLIMIT: i32 = 3;
        const STEP_DUP2: i32 = 4;
        const STEP_CHDIR: i32 = 5;
        const STEP_EXEC: i32 = 6;

        // Macro wrapping unix::child_bail for ergonomic use in the child.
        macro_rules! child_bail {
            ($err_fd:expr, $step:expr, $errno:expr) => {{
                // SAFETY: err_fd is valid; called from forked child
                unsafe { unix::child_bail($err_fd, $step, $errno) }
            }};
        }

        // Start a new session so the child becomes its own process group leader.
        // This lets the parent killpg() all descendants, not just the direct child.
        // setsid() only fails with EPERM if the process is already a session
        // leader, which cannot happen after fork (fresh PID != parent SID).
        // SAFETY: setsid() is async-signal-safe per POSIX, safe to call after fork.
        if unsafe { libc::setsid() } < 0 {
            child_bail!(err_pipe_wr, STEP_SETSID, *libc::__error());
        }

        // Close parent's end of error pipe
        // SAFETY: valid fd
        unsafe { libc::close(err_pipe_rd) };

        // Close parent's stdio pipe ends
        unix::close_parent_pipes(parent_stdin, parent_stdout, parent_stderr);

        // Apply seatbelt profile — permanent, inherited by exec'd process
        if let Err(e) = seatbelt::apply_profile(&profile) {
            child_bail!(
                err_pipe_wr,
                STEP_SEATBELT,
                e.raw_os_error().unwrap_or(libc::EPERM)
            );
        }

        // Apply resource limits
        // SAFETY: single-threaded child, before exec
        if let Err(e) = unsafe { apply_resource_limits(policy) } {
            child_bail!(
                err_pipe_wr,
                STEP_RLIMIT,
                e.raw_os_error().unwrap_or(libc::EPERM)
            );
        }

        // Set up stdio: dup2 the child fds to 0/1/2
        if child_stdin != 0 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stdin, 0) } < 0 {
                child_bail!(err_pipe_wr, STEP_DUP2, *libc::__error());
            }
            // SAFETY: fd is valid
            unsafe { unix::close_if_not_std(child_stdin) };
        }
        if child_stdout != 1 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stdout, 1) } < 0 {
                child_bail!(err_pipe_wr, STEP_DUP2, *libc::__error());
            }
            // SAFETY: fd is valid
            unsafe { unix::close_if_not_std(child_stdout) };
        }
        if child_stderr != 2 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stderr, 2) } < 0 {
                child_bail!(err_pipe_wr, STEP_DUP2, *libc::__error());
            }
            // SAFETY: fd is valid
            unsafe { unix::close_if_not_std(child_stderr) };
        }

        // Change working directory if specified
        if let Some(ref cwd) = prefork.cwd {
            // SAFETY: valid CString pointer
            if unsafe { libc::chdir(cwd.as_ptr()) } != 0 {
                child_bail!(err_pipe_wr, STEP_CHDIR, *libc::__error());
            }
        }

        // err_pipe_wr has O_CLOEXEC: on successful exec, parent reads EOF.

        // SAFETY: program, argv_ptrs, envp_ptrs are all valid null-terminated
        // arrays built in prepare_prefork() before fork.
        unsafe {
            libc::execve(
                prefork.program.as_ptr(),
                prefork.argv_ptrs.as_ptr(),
                prefork.envp_ptrs.as_ptr(),
            );
        }

        // exec failed
        child_bail!(err_pipe_wr, STEP_EXEC, *libc::__error());
    }

    // === PARENT PROCESS ===
    // Close child's ends of pipes
    // SAFETY: these fds are valid
    unsafe {
        libc::close(err_pipe_wr);
        unix::close_if_not_std(child_stdin);
        unix::close_if_not_std(child_stdout);
        unix::close_if_not_std(child_stderr);
    }

    // Check error pipe: if child wrote [step:i32, errno:i32], setup failed
    let mut err_buf = [0u8; 8];
    // SAFETY: err_pipe_rd is valid, err_buf is stack-allocated
    let n = unsafe { libc::read(err_pipe_rd, err_buf.as_mut_ptr().cast(), 8) };
    // SAFETY: valid fd
    unsafe { libc::close(err_pipe_rd) };

    if n == 8 {
        let step = i32::from_ne_bytes([err_buf[0], err_buf[1], err_buf[2], err_buf[3]]);
        let errno = i32::from_ne_bytes([err_buf[4], err_buf[5], err_buf[6], err_buf[7]]);
        let step_name = match step {
            1 => "setsid",
            2 => "seatbelt (sandbox_init)",
            3 => "resource limits (setrlimit)",
            4 => "dup2 (stdio)",
            5 => "chdir",
            6 => "execve",
            _ => "unknown",
        };
        // Reap the child so we don't leak a zombie
        // SAFETY: valid pid
        unsafe { libc::waitpid(child_pid, std::ptr::null_mut(), 0) };
        unix::close_parent_pipes(parent_stdin, parent_stdout, parent_stderr);
        return Err(SandboxError::Setup(format!(
            "child setup failed at step '{}': {}",
            step_name,
            io::Error::from_raw_os_error(errno)
        )));
    }

    Ok(SandboxedChild {
        inner: MacSandboxedChild {
            inner: UnixSandboxedChild {
                pid: child_pid,
                stdin_fd: parent_stdin,
                stdout_fd: parent_stdout,
                stderr_fd: parent_stderr,
                waited: AtomicBool::new(false),
                kill_style: KillStyle::KillProcessGroup,
            },
        },
    })
}

/// A running sandboxed process on macOS.
///
/// Wraps `UnixSandboxedChild` for shared lifecycle methods. macOS uses
/// `killpg` to kill the process group (child called `setsid`).
pub struct MacSandboxedChild {
    inner: UnixSandboxedChild,
}

impl MacSandboxedChild {
    pub const fn id(&self) -> u32 {
        self.inner.id()
    }

    pub fn kill(&mut self) -> io::Result<()> {
        self.inner.kill()
    }

    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        self.inner.wait()
    }

    pub fn try_wait(&self) -> io::Result<Option<std::process::ExitStatus>> {
        self.inner.try_wait()
    }

    pub fn wait_with_output(mut self) -> io::Result<std::process::Output> {
        self.inner.wait_with_output()
    }

    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        self.inner.take_stdin()
    }

    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.inner.take_stdout()
    }

    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        self.inner.take_stderr()
    }

    /// Kill the process group (child called setsid), wait for exit, close fds.
    ///
    /// macOS seatbelt has no post-exit cleanup, so this is just kill + wait.
    /// Consumes `self`; Drop still runs but sees already-cleaned-up state.
    #[allow(clippy::unnecessary_wraps)] // Signature must match SandboxedChild::kill_and_cleanup
    pub fn kill_and_cleanup(mut self) -> crate::Result<()> {
        self.inner.close_fds();
        self.inner.kill_and_reap();
        Ok(())
    }
}

/// Send SIGKILL to the process group by raw PID. Best-effort; the
/// process may have already exited.
#[cfg(feature = "tokio")]
#[allow(unsafe_code)]
pub fn kill_by_pid(pid: u32) {
    let Some(pid_i32) = i32::try_from(pid).ok().filter(|&p| p > 0) else {
        return;
    };
    // macOS children call setsid(), so PGID == PID — negate to kill
    // the entire process group.
    // SAFETY: Sending SIGKILL to a valid negated PGID.
    unsafe {
        libc::kill(-pid_i32, libc::SIGKILL);
    }
}

impl Drop for MacSandboxedChild {
    fn drop(&mut self) {
        self.inner.close_fds();
        self.inner.kill_and_reap();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::command::SandboxStdio;
    use crate::policy::ResourceLimits;
    use std::path::PathBuf;

    fn test_policy(read_paths: Vec<PathBuf>) -> SandboxPolicy {
        SandboxPolicy::new(
            read_paths,
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        )
    }

    #[test]
    fn sandbox_exec_echo_hello() {
        // Verify our generated SBPL profile works via sandbox-exec
        // (bypasses our fork/exec code to test the profile itself).
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let program = std::path::Path::new("/bin/echo");
        let resolved = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
        let profile = seatbelt::generate_profile(&policy, &resolved);

        let out = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/echo", "hello"])
            .output()
            .expect("sandbox-exec");
        assert!(
            out.status.success(),
            "sandbox-exec echo failed: {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.trim(), "hello");
    }

    #[test]
    fn spawn_echo_hello() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.arg("hello");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");

        let out = child.inner.wait_with_output().expect("wait_with_output");
        assert!(
            out.status.success(),
            "echo should succeed: {:?}",
            out.status
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.trim(), "hello");
    }

    #[test]
    fn spawn_read_allowed_path() {
        let tmp = tempfile::TempDir::new().expect("create temp dir");
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "sandbox_test_content").expect("write test file");

        let policy = SandboxPolicy::new(
            vec![tmp.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg(test_file.to_str().expect("path to str"));
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");

        let out = child.inner.wait_with_output().expect("wait_with_output");
        assert!(out.status.success(), "cat should succeed: {:?}", out.status);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("sandbox_test_content"),
            "expected to read allowed file, got: {stdout}"
        );
    }

    #[test]
    fn spawn_read_disallowed_path_fails() {
        // Only allow reading /usr, then try to read from /etc
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg("/etc/hosts");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");

        let out = child.inner.wait_with_output().expect("wait_with_output");
        // cat should fail because /etc is not in read_paths
        assert!(
            !out.status.success(),
            "expected cat /etc/hosts to fail inside sandbox"
        );
        // Must exit normally (not by signal) to confirm policy enforcement
        assert!(
            out.status.code().is_some(),
            "process should exit normally, not by signal: {:?}",
            out.status
        );
    }

    #[test]
    fn generate_profile_produces_valid_sbpl() {
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/tmp/read")],
            vec![PathBuf::from("/tmp/write")],
            vec![PathBuf::from("/usr/bin")],
            vec![],
            true,
            ResourceLimits::default(),
        );
        let program = std::path::PathBuf::from("/usr/bin/test");
        let profile = seatbelt::generate_profile(&policy, &program);
        assert!(profile.starts_with("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read* (subpath \"/tmp/read\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/tmp/write\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp/write\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/usr/bin\"))"));
        assert!(profile.contains("(allow network*)"));
    }
}
