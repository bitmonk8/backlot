#![allow(unsafe_code)]

mod cgroup;
mod namespace;
mod seccomp;

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::AtomicBool;

use crate::command::SandboxCommand;
use crate::policy::SandboxPolicy;
use crate::unix;
use crate::unix::{KillStyle, UnixSandboxedChild};
use crate::{PlatformCapabilities, Result, SandboxError, SandboxedChild};

use cgroup::CgroupGuard;

/// Directories Linux makes accessible to sandboxed processes (auto-mounted or always-allowed).
pub fn platform_implicit_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for p in &[
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib32",
        "/bin",
        "/usr/bin",
        "/sbin",
        "/usr/sbin",
        "/usr/local/bin",
    ] {
        let path = std::path::Path::new(p);
        if path.exists() {
            paths.push(path.to_path_buf());
        }
    }
    paths
}

pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: namespace::is_available(),
        seccomp: seccomp::is_available(),
        cgroups_v2: cgroup::is_available(),
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

/// Close all file descriptors >= 3 except those in `keep_fds`, using the
/// `close_range` syscall (Linux 5.9+). This prevents inherited write-open fds
/// from other threads from causing ETXTBSY when a sibling process calls execve.
///
/// On kernels < 5.9, `close_range` fails silently and inherited fds are NOT
/// closed (they leak). This is a known limitation: the ETXTBSY race remains
/// possible but spawn still works correctly.
///
/// # Safety
/// Must only be called in a single-threaded forked child (the helper process).
/// At most 8 fds >= 3 may appear in `keep_fds`; excess entries are dropped (debug_assert in debug builds).
unsafe fn close_inherited_fds(keep_fds: &[i32]) {
    let mut sorted = [0i32; 8];
    let mut len = 0usize;
    for &fd in keep_fds {
        if fd >= 3 && len < sorted.len() {
            sorted[len] = fd;
            len += 1;
        }
    }
    debug_assert!(
        keep_fds.iter().filter(|&&fd| fd >= 3).count() <= 8,
        "close_inherited_fds: more than 8 fds >= 3 passed"
    );
    // No heap allocation post-fork (allocator may be in inconsistent state).
    for i in 1..len {
        let mut j = i;
        while j > 0 && sorted[j - 1] > sorted[j] {
            sorted.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut deduped = [0i32; 8];
    let mut dlen = 0usize;
    for &fd in sorted.iter().take(len) {
        if dlen == 0 || fd != deduped[dlen - 1] {
            deduped[dlen] = fd;
            dlen += 1;
        }
    }

    // Close fd ranges in the gaps between kept fds (inclusive bounds).
    let mut start: u32 = 3;
    for &keep_fd in deduped.iter().take(dlen) {
        let keep = keep_fd as u32;
        if start < keep {
            // SAFETY: helper is single-threaded; closing stray inherited fds is safe.
            unsafe { libc::syscall(libc::SYS_close_range, start, keep - 1, 0u32) };
        }
        start = keep + 1;
    }
    // SAFETY: same as above; u32::MAX means "up to the highest possible fd".
    unsafe { libc::syscall(libc::SYS_close_range, start, u32::MAX, 0u32) };
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
    // Step names are 1-indexed, matching the STEP_* constants in the child.
    const STEP_NAMES: &[&str] = &[
        "unshare",                      // 1
        "user namespace (uid/gid map)", // 2
        "mount namespace",              // 3
        "inner fork",                   // 4
        "dup2 (stdio)",                 // 5
        "chdir",                        // 6
        "mount /proc",                  // 7
        "pivot_root",                   // 8
        "seccomp",                      // 9
        "execve",                       // 10
        "cgroup join",                  // 11
        "prctl(PR_SET_PDEATHSIG)",      // 12
    ];

    // Create cgroup before forking so the helper can move itself into it.
    // If the policy requests resource limits and cgroup setup fails, return
    // an error rather than silently dropping the limits.
    let cgroup_guard = if policy.limits().has_any() {
        if !cgroup::is_available() {
            return Err(SandboxError::Setup(
                "resource limits requested but cgroups v2 unavailable".into(),
            ));
        }
        Some(
            CgroupGuard::new(policy.limits())
                .map_err(|e| SandboxError::Setup(format!("cgroup creation failed: {e}")))?,
        )
    } else {
        None
    };

    let prefork = prepare_prefork(command, cgroup_guard.as_ref())
        .map_err(|e| SandboxError::Setup(format!("pre-fork preparation: {e}")))?;

    // Build seccomp filter before forking (allocates)
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    let bpf_filter = seccomp::build_filter(policy)
        .map_err(|e| SandboxError::Setup(format!("seccomp filter build: {e}")))?;

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

        // Step constants for error reporting
        const STEP_UNSHARE: i32 = 1;
        const STEP_USER_NS: i32 = 2;
        const STEP_MOUNT_NS: i32 = 3;
        const STEP_FORK_INNER: i32 = 4;
        const STEP_DUP2: i32 = 5;
        const STEP_CHDIR: i32 = 6;
        const STEP_MOUNT_PROC: i32 = 7;
        const STEP_PIVOT_ROOT: i32 = 8;
        const STEP_SECCOMP: i32 = 9;
        const STEP_EXEC: i32 = 10;
        const STEP_CGROUP: i32 = 11;
        const STEP_PDEATHSIG: i32 = 12;

        // Macro wrapping unix::child_bail for ergonomic use in the child.
        macro_rules! child_bail {
            ($err_fd:expr, $step:expr, $errno:expr) => {{
                // SAFETY: err_fd is valid; called from forked child
                unsafe { unix::child_bail($err_fd, $step, $errno) }
            }};
        }

        // Close parent's ends of pipes
        // SAFETY: these fds are valid
        unsafe {
            libc::close(err_pipe_rd);
        }

        // Close parent's stdio pipe ends (not the child's — those are needed for inner child)
        unix::close_pipe_fds(parent_stdin, parent_stdout, parent_stderr);

        // Close all inherited fds except the ones the helper/inner-child need.
        // This prevents write-open fds from other threads (e.g. std::fs::copy)
        // from being held open, which would cause ETXTBSY on execve in sibling
        // processes.
        // SAFETY: helper is single-threaded after fork; listed fds are valid.
        unsafe {
            close_inherited_fds(&[err_pipe_wr, child_stdin, child_stdout, child_stderr]);
        }

        // Unshare namespaces. Skip CLONE_NEWNET when network access is allowed
        // so the child inherits the parent's network namespace.
        let mut clone_flags =
            libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWPID | libc::CLONE_NEWIPC;
        if !policy.allow_network() {
            clone_flags |= libc::CLONE_NEWNET;
        }
        // SAFETY: unshare is async-signal-safe in practice; helper is
        // single-threaded so CLONE_NEWUSER is permitted.
        let rc = unsafe { libc::unshare(clone_flags) };
        if rc != 0 {
            // Capture errno immediately — subsequent calls could clobber it.
            // SAFETY: errno access has no preconditions.
            let saved_errno = unsafe { *libc::__errno_location() };
            child_bail!(err_pipe_wr, STEP_UNSHARE, saved_errno);
        }

        // Set up UID/GID mapping — must happen before mount namespace setup.
        // We can use fs::write here because the helper is single-threaded and
        // hasn't exec'd yet. The allocator is safe in a single-threaded forked
        // process that doesn't use the Rust async runtime.
        if let Err(e) = namespace::setup_user_namespace(prefork.uid, prefork.gid) {
            child_bail!(
                err_pipe_wr,
                STEP_USER_NS,
                e.raw_os_error().unwrap_or(libc::EPERM)
            );
        }

        // Set up mount namespace (bind mounts, dev nodes — but NOT /proc or pivot_root)
        let new_root = match namespace::setup_mount_namespace(policy) {
            Ok(root) => root,
            Err(e) => {
                child_bail!(
                    err_pipe_wr,
                    STEP_MOUNT_NS,
                    e.raw_os_error().unwrap_or(libc::EPERM)
                );
            }
        };

        // Move helper into the cgroup so both helper and inner child inherit
        // the resource limits. Uses pre-built CString path to avoid allocation.
        // Failure is fatal: the cgroup was successfully created and the
        // process should join it to enforce the requested resource limits.
        if let Some(ref procs_path) = prefork.cgroup_procs_path {
            // SAFETY: getpid has no preconditions
            let my_pid = unsafe { libc::getpid() };
            // Stack-based integer-to-ASCII to avoid heap allocation post-fork.
            let mut buf = [0u8; 20];
            let pid_bytes = itoa_stack(my_pid as u64, &mut buf);
            // SAFETY: open() with a valid CString path
            let fd = unsafe { libc::open(procs_path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                let saved_errno = unsafe { *libc::__errno_location() };
                child_bail!(err_pipe_wr, STEP_CGROUP, saved_errno);
            }
            // SAFETY: fd is valid, pid_bytes points into stack-allocated buf
            let written = unsafe { libc::write(fd, pid_bytes.as_ptr().cast(), pid_bytes.len()) };
            // Save errno before close() can clobber it.
            // SAFETY: errno access has no preconditions
            let write_errno = unsafe { *libc::__errno_location() };
            // SAFETY: fd is valid
            unsafe { libc::close(fd) };
            if written < 0 {
                child_bail!(err_pipe_wr, STEP_CGROUP, write_errno);
            }
        }

        // Fork again to enter the PID namespace (inner child gets PID 1 inside)
        // SAFETY: standard fork, helper is single-threaded
        let inner_pid = unsafe { libc::fork() };
        if inner_pid < 0 {
            let saved_errno = unsafe { *libc::__errno_location() };
            child_bail!(err_pipe_wr, STEP_FORK_INNER, saved_errno);
        }

        if inner_pid == 0 {
            // === INNER CHILD (PID 1 inside namespace) ===

            // Save parent PID before prctl to detect the race where the
            // helper dies between fork() and prctl(). If that happens,
            // PR_SET_PDEATHSIG has no effect because the reparenting
            // already occurred.
            // SAFETY: getppid has no preconditions
            let expected_ppid = unsafe { libc::getppid() };

            // Ensure the inner child is killed when the helper dies.
            // The helper used unshare(CLONE_NEWPID), so it is NOT PID 1 in the
            // new namespace — the inner child is. Without this, killing the
            // helper would orphan the inner child (reparented to system init)
            // instead of collapsing the PID namespace.
            // SAFETY: PR_SET_PDEATHSIG is a well-known prctl operation.
            // The signal setting is preserved across execve.
            if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
                let saved_errno = unsafe { *libc::__errno_location() };
                child_bail!(err_pipe_wr, STEP_PDEATHSIG, saved_errno);
            }

            // If the helper died between fork() and prctl(), we were
            // reparented and the death signal will never fire. Detect by
            // checking if our parent PID changed.
            // SAFETY: getppid has no preconditions
            if unsafe { libc::getppid() } != expected_ppid {
                // SAFETY: terminates the forked child
                unsafe { libc::_exit(1) };
            }

            // Set up stdio: dup2 the child fds to 0/1/2
            // SAFETY: all fds are valid, single-threaded forked child
            if let Err(errno) =
                unsafe { unix::setup_stdio_fds(child_stdin, child_stdout, child_stderr) }
            {
                child_bail!(err_pipe_wr, STEP_DUP2, errno);
            }

            // Mount /proc — must happen in the inner child (inside PID
            // namespace) and BEFORE pivot_root, otherwise the lazy-unmounted
            // old root leaves stale procfs entries that cause
            // mnt_already_visible() to reject the mount.
            if let Err(e) = namespace::mount_proc_in_new_root(&new_root) {
                child_bail!(
                    err_pipe_wr,
                    STEP_MOUNT_PROC,
                    e.raw_os_error().unwrap_or(libc::EPERM)
                );
            }

            // Pivot into the new root and unmount the old root.
            if let Err(e) = namespace::pivot_root(&new_root) {
                child_bail!(
                    err_pipe_wr,
                    STEP_PIVOT_ROOT,
                    e.raw_os_error().unwrap_or(libc::EPERM)
                );
            }

            // Change working directory if specified. Must happen AFTER
            // pivot_root() because it does chdir("/") which would overwrite
            // any earlier chdir. After pivot_root,
            // bind-mounted policy paths are at their original absolute
            // locations in the new filesystem.
            if let Some(ref cwd) = prefork.base.cwd {
                // SAFETY: valid CString pointer
                if unsafe { libc::chdir(cwd.as_ptr()) } != 0 {
                    let saved_errno = unsafe { *libc::__errno_location() };
                    child_bail!(err_pipe_wr, STEP_CHDIR, saved_errno);
                }
            }

            // Apply seccomp filter (last step before exec — seccomp must be
            // applied after all setup syscalls are complete)
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            {
                if let Err(e) = seccomp::apply_filter(&bpf_filter) {
                    child_bail!(
                        err_pipe_wr,
                        STEP_SECCOMP,
                        e.raw_os_error().unwrap_or(libc::EPERM)
                    );
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
            child_bail!(err_pipe_wr, STEP_EXEC, *libc::__errno_location());
        }

        // === HELPER continues (inner_pid > 0) ===

        // Close error pipe — helper doesn't need it anymore
        // SAFETY: valid fd
        unsafe { libc::close(err_pipe_wr) };

        // Wait for inner child, retrying on EINTR
        let mut inner_status: libc::c_int = 0;
        loop {
            // SAFETY: valid pid, valid pointer
            let ret = unsafe { libc::waitpid(inner_pid, &raw mut inner_status, 0) };
            if ret != -1 {
                break;
            }
            // SAFETY: reading thread-local errno after failed syscall
            let e = unsafe { *libc::__errno_location() };
            if e == libc::EINTR {
                continue;
            }
            // Non-EINTR error — cannot recover the child's status
            // SAFETY: terminates the helper process
            unsafe { libc::_exit(127) };
        }

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

    // SAFETY: err_pipe_rd is valid, helper_pid is valid
    unsafe {
        unix::check_child_error_pipe(
            err_pipe_rd,
            helper_pid,
            parent_stdin,
            parent_stdout,
            parent_stderr,
            STEP_NAMES,
        )?;
    }

    Ok(SandboxedChild {
        inner: LinuxSandboxedChild {
            inner: UnixSandboxedChild {
                pid: helper_pid,
                stdin_fd: parent_stdin,
                stdout_fd: parent_stdout,
                stderr_fd: parent_stderr,
                waited: AtomicBool::new(false),
                kill_style: KillStyle::Single,
            },
            cgroup_guard,
        },
    })
}

/// A running sandboxed process on Linux.
///
/// Wraps `UnixSandboxedChild` for shared lifecycle methods and adds
/// Linux-specific cgroup cleanup. The helper PID is the target for
/// kill/wait; the inner child dies via `PR_SET_PDEATHSIG` when the
/// helper exits.
pub struct LinuxSandboxedChild {
    inner: UnixSandboxedChild,
    /// Held until drop to enforce resource limits and clean up the cgroup.
    cgroup_guard: Option<CgroupGuard>,
}

impl LinuxSandboxedChild {
    unix::delegate_unix_child_methods!(inner);

    /// Kill the helper (and by extension, all namespaced descendants),
    /// wait for it to exit, close fds, and drop the cgroup guard.
    ///
    /// Consumes `self`; Drop still runs but sees already-cleaned-up state.
    #[allow(clippy::unnecessary_wraps)] // Signature must match SandboxedChild::kill_and_cleanup
    pub fn kill_and_cleanup(mut self) -> crate::Result<()> {
        self.inner.close_fds();
        self.inner.kill_and_reap();

        // CgroupGuard::drop handles kill + rmdir when self is dropped.
        // Taking it here and dropping explicitly makes the ordering clear.
        drop(self.cgroup_guard.take());

        Ok(())
    }
}

/// Send SIGKILL to the helper process by raw PID. Best-effort; the
/// process may have already exited. Killing the helper collapses the
/// PID namespace (inner child has PR_SET_PDEATHSIG).
#[cfg(feature = "tokio")]
#[allow(unsafe_code)]
pub fn kill_by_pid(pid: u32) {
    let Some(pid_i32) = unix::validate_kill_pid(pid) else {
        return;
    };
    // SAFETY: Sending SIGKILL to a valid pid.
    unsafe {
        libc::kill(pid_i32, libc::SIGKILL);
    }
}

impl Drop for LinuxSandboxedChild {
    fn drop(&mut self) {
        self.inner.close_fds();
        self.inner.kill_and_reap();
    }
}

#[cfg(test)]
pub mod test_helpers {
    /// Create a pipe using libc, returns (read_fd, write_fd).
    pub fn make_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        // SAFETY: fds is a valid 2-element array
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        (fds[0], fds[1])
    }

    /// Write bytes to an fd. Ignores return value to remain
    /// async-signal-safe (no panic/assert in forked children).
    pub fn write_fd(fd: i32, data: &[u8]) {
        // SAFETY: fd is valid, data pointer and len are correct
        unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    }

    /// Read all available bytes from an fd into a String.
    pub fn read_fd_to_string(fd: i32) -> String {
        let mut buf = [0u8; 256];
        // SAFETY: fd is valid, buf pointer and len are correct
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::test_helpers::{read_fd_to_string, write_fd};
    use super::*;
    use crate::ResourceLimits;
    use crate::command::SandboxStdio;
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
    fn spawn_echo_hello() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.arg("hello");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");

        let out = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.trim(), "hello");
    }

    #[test]
    fn spawn_pid1_in_namespace() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg("/proc/self/status");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let output = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        // The PID namespace child should see itself as PID 1
        let mut found_pid = false;
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix("Pid:") {
                let pid_str = rest.trim();
                assert_eq!(
                    pid_str, "1",
                    "expected PID 1 inside namespace, got {pid_str}"
                );
                found_pid = true;
                break;
            }
        }
        assert!(
            found_pid,
            "expected to find Pid: line in /proc/self/status output"
        );
    }

    #[test]
    fn spawn_proc_mounted() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/ls");
        cmd.arg("/proc");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let output = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");

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

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let output = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");

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

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let output = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");

        // /home should not be accessible — ls should fail
        assert!(
            !output.status.success(),
            "expected ls /home to fail inside sandbox"
        );
    }

    fn is_fd_open(fd: i32) -> bool {
        // SAFETY: fcntl F_GETFD is a read-only query
        unsafe { libc::fcntl(fd, libc::F_GETFD) >= 0 }
    }

    #[test]
    fn close_inherited_fds_preserves_kept_fds() {
        let (r1, w1) = crate::unix::make_pipe().expect("pipe");
        let (r2, w2) = crate::unix::make_pipe().expect("pipe");
        let (res_r, res_w) = crate::unix::make_pipe().expect("pipe");

        // SAFETY: fork is safe; child only calls async-signal-safe functions.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(res_r) };

            // SAFETY: closes all fds >= 3 except r1, r2, and the result pipe
            unsafe { close_inherited_fds(&[r1, r2, res_w]) };

            let r1_open = is_fd_open(r1);
            let r2_open = is_fd_open(r2);
            let w1_open = is_fd_open(w1);
            let w2_open = is_fd_open(w2);

            if r1_open && r2_open && !w1_open && !w2_open {
                write_fd(res_w, b"OK");
            } else {
                write_fd(res_w, b"FAIL");
            }
            // SAFETY: closing fd before exit
            unsafe { libc::close(res_w) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fds
        unsafe {
            libc::close(res_w);
            libc::close(r1);
            libc::close(w1);
            libc::close(r2);
            libc::close(w2);
        }

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = read_fd_to_string(res_r);
        // SAFETY: closing fd
        unsafe { libc::close(res_r) };

        assert_eq!(result, "OK", "child reported: {result}");
    }

    #[test]
    fn close_inherited_fds_handles_duplicates() {
        let (r, w) = crate::unix::make_pipe().expect("pipe");
        let (res_r, res_w) = crate::unix::make_pipe().expect("pipe");

        // SAFETY: fork is safe; child only calls async-signal-safe functions.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(res_r) };

            // SAFETY: passing same fd twice should not cause issues
            unsafe { close_inherited_fds(&[r, r, res_w]) };

            let r_open = is_fd_open(r);
            let w_open = is_fd_open(w);

            if r_open && !w_open {
                write_fd(res_w, b"OK");
            } else {
                write_fd(res_w, b"FAIL");
            }
            // SAFETY: closing fd before exit
            unsafe { libc::close(res_w) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fds
        unsafe {
            libc::close(res_w);
            libc::close(r);
            libc::close(w);
        }

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = read_fd_to_string(res_r);
        // SAFETY: closing fd
        unsafe { libc::close(res_r) };

        assert_eq!(result, "OK", "child reported: {result}");
    }

    #[test]
    fn close_inherited_fds_empty_keeps() {
        let (r, w) = crate::unix::make_pipe().expect("pipe");
        let (res_r, res_w) = crate::unix::make_pipe().expect("pipe");

        // SAFETY: fork is safe; child only calls async-signal-safe functions.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(res_r) };

            // Keep only the result pipe so we can report back
            // SAFETY: closes all fds >= 3 except the result pipe
            unsafe { close_inherited_fds(&[res_w]) };

            let r_open = is_fd_open(r);
            let w_open = is_fd_open(w);

            if !r_open && !w_open {
                write_fd(res_w, b"OK");
            } else {
                write_fd(res_w, b"FAIL");
            }
            // SAFETY: closing fd before exit
            unsafe { libc::close(res_w) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fds
        unsafe {
            libc::close(res_w);
            libc::close(r);
            libc::close(w);
        }

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = read_fd_to_string(res_r);
        // SAFETY: closing fd
        unsafe { libc::close(res_r) };

        assert_eq!(result, "OK", "child reported: {result}");
    }

    // ── namespace setup coverage ────────────────────────────────────

    #[test]
    fn spawn_has_system_lib_paths() {
        // Verify /usr/lib is accessible inside the sandbox (mount_system_paths coverage).
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/sh");
        cmd.args([
            "-c",
            "test -d /usr/lib && echo SYSLIB_OK || echo SYSLIB_MISSING",
        ]);
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let output = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("SYSLIB_OK"),
            "expected /usr/lib to exist inside sandbox, got: {stdout}"
        );
    }

    #[test]
    fn spawn_runs_as_root_in_namespace() {
        // Verify uid mapping maps to 0 inside the sandbox (setup_user_namespace coverage).
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/usr/bin/id");
        cmd.arg("-u");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let output = child
            .inner
            .wait_with_output()
            .expect("wait_with_output must succeed");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.trim(),
            "0",
            "expected uid 0 inside namespace, got: {stdout}"
        );
    }

    // ── itoa_stack ──────────────────────────────────────────────────

    #[test]
    fn itoa_stack_zero() {
        let mut buf = [0u8; 20];
        let result = itoa_stack(0, &mut buf);
        assert_eq!(result, b"0");
    }

    #[test]
    fn itoa_stack_one() {
        let mut buf = [0u8; 20];
        let result = itoa_stack(1, &mut buf);
        assert_eq!(result, b"1");
    }

    #[test]
    fn itoa_stack_large_number() {
        let mut buf = [0u8; 20];
        let result = itoa_stack(123_456_789, &mut buf);
        assert_eq!(result, b"123456789");
    }

    #[test]
    fn itoa_stack_u64_max() {
        let mut buf = [0u8; 20];
        let result = itoa_stack(u64::MAX, &mut buf);
        assert_eq!(std::str::from_utf8(result).unwrap(), u64::MAX.to_string());
    }

    #[test]
    fn itoa_stack_powers_of_ten() {
        for exp in 0..20u32 {
            let val = 10u64.checked_pow(exp);
            if let Some(v) = val {
                let mut buf = [0u8; 20];
                let result = itoa_stack(v, &mut buf);
                assert_eq!(
                    std::str::from_utf8(result).unwrap(),
                    v.to_string(),
                    "failed for 10^{exp}"
                );
            }
        }
    }

    // ── Drop without cleanup ────────────────────────────────────────

    #[test]
    fn drop_kills_long_running_child() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/sleep");
        cmd.arg("60");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = spawn(&policy, &cmd).expect("spawn must succeed");
        let pid = child.inner.id();
        drop(child);
        // After drop, the process should be gone.
        let proc_path = format!("/proc/{pid}");
        // Give the kernel a moment to reap.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !std::path::Path::new(&proc_path).exists(),
            "process should be gone after drop"
        );
    }

    #[test]
    fn close_inherited_fds_ignores_std_fds() {
        let (r, w) = crate::unix::make_pipe().expect("pipe");
        let (res_r, res_w) = crate::unix::make_pipe().expect("pipe");

        // SAFETY: fork is safe; child only calls async-signal-safe functions.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(res_r) };

            // SAFETY: passing std fds should be filtered out
            unsafe { close_inherited_fds(&[0, 1, 2, r, res_w]) };

            let r_open = is_fd_open(r);

            if r_open {
                write_fd(res_w, b"OK");
            } else {
                write_fd(res_w, b"r_closed");
            }
            // SAFETY: closing fd before exit
            unsafe { libc::close(res_w) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fds
        unsafe {
            libc::close(res_w);
            libc::close(r);
            libc::close(w);
        }

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = read_fd_to_string(res_r);
        // SAFETY: closing fd
        unsafe { libc::close(res_r) };

        assert_eq!(result, "OK", "child reported: {result}");
    }
}
