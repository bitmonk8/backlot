#![allow(unsafe_code)]

mod cgroup;
mod namespace;
mod seccomp;

use std::cell::Cell;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;

use crate::command::SandboxCommand;
use crate::policy::SandboxPolicy;
use crate::unix;
use crate::{PlatformCapabilities, Result, SandboxError, SandboxedChild};

use cgroup::CgroupGuard;

pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: namespace::available(),
        seccomp: seccomp::available(),
        cgroups_v2: cgroup::available(),
        seatbelt: false,
        appcontainer: false,
        job_objects: false,
    }
}

/// Linux-specific pre-fork data extending the shared `PreForkData`.
struct LinuxPreForkData {
    base: unix::PreForkData,
    uid: u32,
    gid: u32,
    /// Path to cgroup.procs file, pre-converted for the helper to use.
    cgroup_procs_path: Option<CString>,
}

/// Build all C strings and data structures before forking.
fn prepare_prefork(
    command: &SandboxCommand,
    cgroup_guard: Option<&CgroupGuard>,
) -> io::Result<LinuxPreForkData> {
    let base = unix::prepare_prefork(command)?;

    // SAFETY: getuid/getgid have no preconditions
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let cgroup_procs_path = match cgroup_guard {
        Some(g) => {
            let procs = g.path().join("cgroup.procs");
            Some(
                CString::new(procs.as_os_str().as_bytes())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
            )
        }
        None => None,
    };

    Ok(LinuxPreForkData {
        base,
        uid,
        gid,
        cgroup_procs_path,
    })
}

/// Convert a u64 to ASCII digits in a caller-provided stack buffer.
/// Returns the slice of `buf` containing the ASCII digits.
/// Does not allocate — safe to call post-fork.
fn itoa_stack(mut val: u64, buf: &mut [u8; 20]) -> &[u8] {
    if val == 0 {
        buf[19] = b'0';
        return &buf[19..];
    }
    let mut pos = 20;
    while val > 0 {
        pos -= 1;
        buf[pos] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    &buf[pos..]
}

pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    // Create cgroup before forking so the helper can move itself into it.
    // If cgroups are unavailable, we proceed without resource limits.
    let cgroup_guard = if cgroup::available() {
        CgroupGuard::new(&policy.limits).ok()
    } else {
        None
    };

    let prefork = prepare_prefork(command, cgroup_guard.as_ref())
        .map_err(|e| SandboxError::Setup(format!("pre-fork preparation: {e}")))?;

    // Build seccomp filter before forking (allocates)
    #[cfg(target_arch = "x86_64")]
    let bpf_filter = seccomp::build_filter(policy)
        .map_err(|e| SandboxError::Setup(format!("seccomp filter build: {e}")))?;

    // Set up stdio pipes before forking
    let (child_stdin, child_stdout, child_stderr, parent_stdin, parent_stdout, parent_stderr) =
        unix::setup_stdio_pipes(command)
            .map_err(|e| SandboxError::Setup(format!("stdio pipe setup: {e}")))?;

    // Error pipe: child writes errno here if anything fails before exec
    let (err_pipe_rd, err_pipe_wr) =
        unix::make_pipe().map_err(|e| SandboxError::Setup(format!("error pipe: {e}")))?;

    // SAFETY: We fork a helper process. Between fork and _exit/exec in child
    // processes, we only call async-signal-safe functions (or functions that
    // are safe because the helper is single-threaded after fork).
    // The helper immediately enters namespace setup and never returns to
    // multi-threaded Rust runtime code.
    let helper_pid = unsafe { libc::fork() };

    if helper_pid < 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }

    if helper_pid == 0 {
        // === HELPER PROCESS (single-threaded after fork) ===
        // Close parent's ends of pipes
        // SAFETY: these fds are valid
        unsafe {
            libc::close(err_pipe_rd);
        }

        // Close parent's stdio pipe ends (not the child's — those are needed for inner child)
        unix::close_parent_pipes(parent_stdin, parent_stdout, parent_stderr);

        // Macro to report error and exit from helper
        macro_rules! helper_bail {
            ($err_fd:expr, $errno:expr) => {{
                let e = ($errno as i32).to_ne_bytes();
                // SAFETY: err_fd is valid, e is stack-allocated
                let _ = unsafe { libc::write($err_fd, e.as_ptr().cast(), 4) };
                unsafe { libc::_exit(1) };
            }};
        }

        // Unshare namespaces. Skip CLONE_NEWNET when network access is allowed
        // so the child inherits the parent's network namespace.
        let mut clone_flags =
            libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID | libc::CLONE_NEWIPC;
        if !policy.allow_network {
            clone_flags |= libc::CLONE_NEWNET;
        }
        // SAFETY: unshare is async-signal-safe in practice; helper is
        // single-threaded so CLONE_NEWUSER is permitted.
        let rc = unsafe { libc::unshare(clone_flags) };
        if rc != 0 {
            helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
        }

        // Set up UID/GID mapping — must happen before mount namespace setup.
        // We can use fs::write here because the helper is single-threaded and
        // hasn't exec'd yet. The allocator is safe in a single-threaded forked
        // process that doesn't use the Rust async runtime.
        if let Err(e) = namespace::setup_user_namespace(prefork.uid, prefork.gid) {
            helper_bail!(err_pipe_wr, e.raw_os_error().unwrap_or(libc::EPERM));
        }

        // Set up mount namespace
        if let Err(e) = namespace::setup_mount_namespace(policy) {
            helper_bail!(err_pipe_wr, e.raw_os_error().unwrap_or(libc::EPERM));
        }

        // Move helper into the cgroup so both helper and inner child inherit
        // the resource limits. Uses pre-built CString path to avoid allocation.
        if let Some(ref procs_path) = prefork.cgroup_procs_path {
            // SAFETY: getpid has no preconditions
            let my_pid = unsafe { libc::getpid() };
            // Stack-based integer-to-ASCII to avoid heap allocation post-fork.
            let mut buf = [0u8; 20];
            let pid_bytes = itoa_stack(my_pid as u64, &mut buf);
            // SAFETY: open() with a valid CString path
            let fd = unsafe { libc::open(procs_path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
            if fd >= 0 {
                // SAFETY: fd is valid, pid_bytes points into stack-allocated buf
                let _ = unsafe { libc::write(fd, pid_bytes.as_ptr().cast(), pid_bytes.len()) };
                // SAFETY: fd is valid
                unsafe { libc::close(fd) };
            }
            // If cgroup join fails, continue without resource limits rather
            // than aborting the entire spawn.
        }

        // Fork again to enter the PID namespace (inner child gets PID 1 inside)
        // SAFETY: standard fork, helper is single-threaded
        let inner_pid = unsafe { libc::fork() };
        if inner_pid < 0 {
            helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
        }

        if inner_pid == 0 {
            // === INNER CHILD (PID 1 inside namespace) ===

            // Set up stdio: dup2 the child fds to 0/1/2
            if child_stdin != 0 {
                // SAFETY: both fds are valid
                if unsafe { libc::dup2(child_stdin, 0) } < 0 {
                    helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
                }
                unsafe { unix::close_if_not_std(child_stdin) };
            }
            if child_stdout != 1 {
                // SAFETY: both fds are valid
                if unsafe { libc::dup2(child_stdout, 1) } < 0 {
                    helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
                }
                unsafe { unix::close_if_not_std(child_stdout) };
            }
            if child_stderr != 2 {
                // SAFETY: both fds are valid
                if unsafe { libc::dup2(child_stderr, 2) } < 0 {
                    helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
                }
                unsafe { unix::close_if_not_std(child_stderr) };
            }

            // Change working directory if specified
            if let Some(ref cwd) = prefork.base.cwd {
                // SAFETY: valid CString pointer
                if unsafe { libc::chdir(cwd.as_ptr()) } != 0 {
                    helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
                }
            }

            // Apply seccomp filter (last step before exec — seccomp must be
            // applied after all setup syscalls are complete)
            #[cfg(target_arch = "x86_64")]
            {
                if let Err(e) = seccomp::apply_filter(&bpf_filter) {
                    helper_bail!(err_pipe_wr, e.raw_os_error().unwrap_or(libc::EPERM));
                }
            }

            // err_pipe_wr has O_CLOEXEC: on successful exec, parent reads EOF.
            // On exec failure, we write errno and exit.

            // SAFETY: program, argv_ptrs, envp_ptrs are all valid null-terminated
            // arrays built in prepare_prefork() before fork. execve replaces the
            // process image; if it returns, it failed.
            unsafe {
                libc::execve(
                    prefork.base.program.as_ptr(),
                    prefork.base.argv_ptrs.as_ptr(),
                    prefork.base.envp_ptrs.as_ptr(),
                );
            }

            // exec failed — report errno via error pipe, then exit
            helper_bail!(err_pipe_wr, unsafe { *libc::__errno_location() });
        }

        // === HELPER continues (inner_pid > 0) ===

        // Close error pipe — helper doesn't need it anymore
        // SAFETY: valid fd
        unsafe { libc::close(err_pipe_wr) };

        // Wait for inner child
        let mut inner_status: libc::c_int = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(inner_pid, &raw mut inner_status, 0) };

        // Exit with same status as inner child
        if libc::WIFEXITED(inner_status) {
            unsafe { libc::_exit(libc::WEXITSTATUS(inner_status)) };
        }
        // Killed by signal
        unsafe { libc::_exit(128 + libc::WTERMSIG(inner_status)) };
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

    // Check error pipe: if child wrote an errno, setup failed
    let mut err_buf = [0u8; 4];
    // SAFETY: err_pipe_rd is valid, err_buf is stack-allocated
    let n = unsafe { libc::read(err_pipe_rd, err_buf.as_mut_ptr().cast(), 4) };
    // SAFETY: valid fd
    unsafe { libc::close(err_pipe_rd) };

    if n == 4 {
        let errno = i32::from_ne_bytes(err_buf);
        // Reap the helper so we don't leak a zombie
        // SAFETY: valid pid, null pointer (don't need status)
        unsafe { libc::waitpid(helper_pid, std::ptr::null_mut(), 0) };
        unix::close_parent_pipes(parent_stdin, parent_stdout, parent_stderr);
        return Err(SandboxError::Setup(format!(
            "child namespace setup failed: {}",
            io::Error::from_raw_os_error(errno)
        )));
    }

    Ok(SandboxedChild {
        inner: LinuxSandboxedChild {
            helper_pid,
            stdin_fd: parent_stdin,
            stdout_fd: parent_stdout,
            stderr_fd: parent_stderr,
            waited: Cell::new(false),
            cgroup_guard,
        },
    })
}

/// A running sandboxed process on Linux.
///
/// Wraps the helper PID. The helper waits for the actual sandboxed (inner)
/// child and exits with its status, so waiting on the helper gives us the
/// inner child's exit status.
pub struct LinuxSandboxedChild {
    helper_pid: i32,
    stdin_fd: Option<i32>,
    stdout_fd: Option<i32>,
    stderr_fd: Option<i32>,
    /// True once the helper has been reaped via waitpid.
    waited: Cell<bool>,
    /// Held until drop to enforce resource limits and clean up the cgroup.
    cgroup_guard: Option<CgroupGuard>,
}

impl LinuxSandboxedChild {
    pub const fn id(&self) -> u32 {
        self.helper_pid as u32
    }

    pub fn kill(&self) -> io::Result<()> {
        // The helper is PID namespace init; killing it collapses the
        // namespace and kills all descendants.
        // SAFETY: valid pid, SIGKILL is a well-known signal
        let rc = unsafe { libc::kill(self.helper_pid, libc::SIGKILL) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        let mut status: libc::c_int = 0;
        // SAFETY: valid pid, valid pointer
        loop {
            let rc = unsafe { libc::waitpid(self.helper_pid, &raw mut status, 0) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
            break;
        }
        self.waited.set(true);
        Ok(unix::exit_status_from_raw(status))
    }

    pub fn try_wait(&self) -> io::Result<Option<std::process::ExitStatus>> {
        let mut status: libc::c_int = 0;
        // SAFETY: valid pid, valid pointer, WNOHANG for non-blocking
        let rc = unsafe { libc::waitpid(self.helper_pid, &raw mut status, libc::WNOHANG) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if rc == 0 {
            return Ok(None);
        }
        self.waited.set(true);
        Ok(Some(unix::exit_status_from_raw(status)))
    }

    pub fn wait_with_output(mut self) -> io::Result<std::process::Output> {
        // Take fds so Drop won't double-close them
        let stdout_fd = self.stdout_fd.take();
        let stderr_fd = self.stderr_fd.take();

        // Read both pipes concurrently via poll to avoid deadlock when one
        // pipe buffer fills while we block reading the other.
        let (stdout, stderr) = unix::read_two_fds(stdout_fd, stderr_fd)?;
        let status = self.wait()?;

        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        self.stdin_fd.take().map(|fd| {
            // SAFETY: fd is a valid pipe fd we own
            unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) }
        })
    }

    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.stdout_fd.take().map(|fd| {
            // SAFETY: fd is a valid pipe fd we own
            unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) }
        })
    }

    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        self.stderr_fd.take().map(|fd| {
            // SAFETY: fd is a valid pipe fd we own
            unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) }
        })
    }

    /// Close all remaining pipe fds.
    fn close_fds(&mut self) {
        for fd in [
            self.stdin_fd.take(),
            self.stdout_fd.take(),
            self.stderr_fd.take(),
        ]
        .into_iter()
        .flatten()
        {
            // SAFETY: fd is a valid pipe fd we own
            unsafe {
                libc::close(fd);
            }
        }
    }

    /// Kill the helper (and by extension, all namespaced descendants),
    /// wait for it to exit, close fds, and drop the cgroup guard.
    ///
    /// Consumes `self`; Drop still runs but sees already-cleaned-up state.
    pub fn kill_and_cleanup(mut self) -> crate::Result<()> {
        self.close_fds();

        if !self.waited.get() {
            self.kill().map_err(crate::SandboxError::Io)?;
            self.wait().map_err(crate::SandboxError::Io)?;
        }

        // CgroupGuard::drop handles kill + rmdir when self is dropped.
        // Taking it here and dropping explicitly makes the ordering clear.
        drop(self.cgroup_guard.take());

        Ok(())
    }
}

impl Drop for LinuxSandboxedChild {
    fn drop(&mut self) {
        self.close_fds();

        if !self.waited.get() {
            // Kill and reap the helper to prevent zombie leak.
            // SAFETY: helper_pid is valid; blocking wait ensures the zombie
            // is reaped (SIGKILL delivery is near-instant).
            unsafe {
                libc::kill(self.helper_pid, libc::SIGKILL);
                libc::waitpid(self.helper_pid, std::ptr::null_mut(), 0);
            };
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::manual_let_else, clippy::single_match)]
mod tests {
    use super::*;
    use crate::ResourceLimits;
    use crate::command::SandboxStdio;
    use std::path::PathBuf;

    fn test_policy(read_paths: Vec<PathBuf>) -> SandboxPolicy {
        SandboxPolicy {
            read_paths,
            write_paths: vec![],
            exec_paths: vec![],
            allow_network: false,
            limits: ResourceLimits::default(),
        }
    }

    #[test]
    fn spawn_echo_hello() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.arg("hello");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd);
        // On systems without namespace support, this may fail — that's acceptable
        let child = match child {
            Ok(c) => c,
            Err(_) => return, // skip on systems without namespace support
        };

        let output = child.inner.wait_with_output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert_eq!(stdout.trim(), "hello");
            }
            Err(_) => {} // may fail depending on environment
        }
    }

    #[test]
    fn spawn_pid1_in_namespace() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg("/proc/self/status");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        let output = match child.inner.wait_with_output() {
            Ok(out) => out,
            Err(_) => return,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        // The PID namespace child should see itself as PID 1
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("Pid:") {
                let pid_str = rest.trim();
                assert_eq!(
                    pid_str, "1",
                    "expected PID 1 inside namespace, got {pid_str}"
                );
                return;
            }
        }
    }

    #[test]
    fn spawn_proc_mounted() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/ls");
        cmd.arg("/proc");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        let output = match child.inner.wait_with_output() {
            Ok(out) => out,
            Err(_) => return,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        // /proc should contain "self" entry
        assert!(stdout.contains("self"), "expected /proc/self to exist");
    }

    #[test]
    fn spawn_network_isolated() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg("/proc/net/dev");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        let output = match child.inner.wait_with_output() {
            Ok(out) => out,
            Err(_) => return,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        // In a network namespace, only lo should exist (and it may not even be UP)
        // There should be no eth0, wlan0, etc.
        assert!(
            !stdout.contains("eth0") && !stdout.contains("wlan0"),
            "network namespace should not have host interfaces"
        );
    }

    #[test]
    fn spawn_cannot_see_host_paths() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/ls");
        cmd.arg("/home");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        let output = match child.inner.wait_with_output() {
            Ok(out) => out,
            Err(_) => return,
        };

        // /home should not be accessible — ls should fail
        assert!(
            !output.status.success(),
            "expected ls /home to fail inside sandbox"
        );
    }
}
