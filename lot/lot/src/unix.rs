//! Shared infrastructure for Unix backends (Linux, macOS).
//!
//! Contains pipe creation, stdio setup, concurrent pipe reading, fd helpers,
//! `ExitStatus` conversion, the `child_bail` error-reporting function, and
//! the `UnixSandboxedChild` lifecycle struct that are identical across
//! Linux and macOS.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::command::{SandboxCommand, SandboxStdio};

/// Pre-fork data common to both Linux and macOS: C strings for execve,
/// environment, and working directory. Built before fork() so no
/// allocations happen in the child.
pub struct PreForkData {
    pub program: CString,
    #[allow(dead_code)] // kept alive so argv_ptrs remain valid
    pub argv: Vec<CString>,
    #[allow(dead_code)] // kept alive so envp_ptrs remain valid
    pub envp: Vec<CString>,
    /// Pre-built pointer array for execve argv (null-terminated).
    pub argv_ptrs: Vec<*const libc::c_char>,
    /// Pre-built pointer array for execve envp (null-terminated).
    pub envp_ptrs: Vec<*const libc::c_char>,
    pub cwd: Option<CString>,
}

/// Build all C strings and data structures before forking.
pub fn prepare_prefork(command: &SandboxCommand) -> io::Result<PreForkData> {
    let program = CString::new(command.program.as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let mut argv = Vec::with_capacity(1 + command.args.len());
    argv.push(program.clone());
    for arg in &command.args {
        argv.push(
            CString::new(arg.as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
        );
    }

    // Build envp: combine user-supplied env with minimal defaults
    let mut has_path = false;
    let mut envp: Vec<CString> = Vec::with_capacity(command.env.len() + 1);
    for (k, v) in &command.env {
        if k == "PATH" {
            has_path = true;
        }
        let mut entry = Vec::with_capacity(k.len() + 1 + v.len());
        entry.extend_from_slice(k.as_bytes());
        entry.push(b'=');
        entry.extend_from_slice(v.as_bytes());
        envp.push(CString::new(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?);
    }
    if !has_path {
        let mut entry = Vec::from(b"PATH=" as &[u8]);
        entry.extend_from_slice(crate::env_check::DEFAULT_UNIX_PATH.as_bytes());
        envp.push(CString::new(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?);
    }

    let cwd = match &command.cwd {
        Some(p) => Some(
            CString::new(p.as_os_str().as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
        ),
        None => None,
    };

    // Build pointer arrays before fork to avoid post-fork allocations.
    let argv_ptrs: Vec<*const libc::c_char> = argv
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp_ptrs: Vec<*const libc::c_char> = envp
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    Ok(PreForkData {
        program,
        argv,
        envp,
        argv_ptrs,
        envp_ptrs,
        cwd,
    })
}

/// Open `/dev/null` with the given flags (e.g., `O_RDONLY` or `O_WRONLY`).
/// Returns the fd with `O_CLOEXEC` set.
pub fn open_dev_null(flags: i32) -> io::Result<i32> {
    let c_path =
        CString::new("/dev/null").map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: valid path, caller-provided flags | O_CLOEXEC
    let fd = unsafe { libc::open(c_path.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

/// Create a pipe pair with `O_CLOEXEC`. Returns `(read_fd, write_fd)`.
///
/// Uses `pipe2` on Linux (atomic), `pipe` + `fcntl` on non-Linux Unix.
pub fn make_pipe() -> io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];

    #[cfg(target_os = "linux")]
    {
        // SAFETY: fds is a valid 2-element array; O_CLOEXEC prevents fd leak
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: fds is a valid 2-element array
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // Set CLOEXEC on both ends so they don't leak to exec'd process.
        // Fail hard if fcntl fails — a leaked fd could reach the child.
        let close_both = || unsafe {
            // SAFETY: both fds are valid from pipe()
            libc::close(fds[0]);
            libc::close(fds[1]);
        };
        for &fd in &fds {
            // SAFETY: fd is valid from pipe()
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 {
                let err = io::Error::last_os_error();
                close_both();
                return Err(err);
            }
            // SAFETY: fd is valid, setting FD_CLOEXEC
            if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
                let err = io::Error::last_os_error();
                close_both();
                return Err(err);
            }
        }
    }

    let [r, w] = fds;
    Ok((r, w))
}

/// Pipe file descriptors for child and parent sides of stdio.
pub struct StdioPipes {
    pub child_stdin: i32,
    pub child_stdout: i32,
    pub child_stderr: i32,
    pub parent_stdin: Option<i32>,
    pub parent_stdout: Option<i32>,
    pub parent_stderr: Option<i32>,
}

/// Close child fds (if not stdin/stdout/stderr) and parent pipe fds on error.
///
/// # Safety
/// Each fd in `child_fds` must be valid or a standard fd (0/1/2).
unsafe fn cleanup_stdio_fds(child_fds: &[i32], parent_fds: &[Option<i32>]) {
    for &fd in child_fds {
        // SAFETY: caller guarantees fds are valid
        unsafe { close_if_not_std(fd) };
    }
    for f in parent_fds.iter().copied().flatten() {
        // SAFETY: fd is a valid pipe fd from make_pipe()
        unsafe { libc::close(f) };
    }
}

/// Set up stdio for the child process. Creates pipes as needed.
pub fn setup_stdio_pipes(command: &SandboxCommand) -> io::Result<StdioPipes> {
    let (child_stdin, parent_stdin) = match command.stdin {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe()?;
            (r, Some(w))
        }
        SandboxStdio::Null => (open_dev_null(libc::O_RDONLY)?, None),
        SandboxStdio::Inherit => (0, None),
    };

    let (child_stdout, parent_stdout) = match command.stdout {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe().inspect_err(|_| {
                // SAFETY: fds are valid from the successful stdin step above
                unsafe { cleanup_stdio_fds(&[child_stdin], &[parent_stdin]) };
            })?;
            (w, Some(r))
        }
        SandboxStdio::Null => (
            open_dev_null(libc::O_WRONLY).inspect_err(|_| {
                unsafe { cleanup_stdio_fds(&[child_stdin], &[parent_stdin]) };
            })?,
            None,
        ),
        SandboxStdio::Inherit => (1, None),
    };

    let (child_stderr, parent_stderr) = match command.stderr {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe().inspect_err(|_| {
                // SAFETY: fds are valid from successful steps above
                unsafe {
                    cleanup_stdio_fds(&[child_stdin, child_stdout], &[parent_stdin, parent_stdout]);
                }
            })?;
            (w, Some(r))
        }
        SandboxStdio::Null => (
            open_dev_null(libc::O_WRONLY).inspect_err(|_| unsafe {
                cleanup_stdio_fds(&[child_stdin, child_stdout], &[parent_stdin, parent_stdout]);
            })?,
            None,
        ),
        SandboxStdio::Inherit => (2, None),
    };

    Ok(StdioPipes {
        child_stdin,
        child_stdout,
        child_stderr,
        parent_stdin,
        parent_stdout,
        parent_stderr,
    })
}

/// Close an fd if it's not one of the standard fds (0, 1, 2).
///
/// # Safety
/// `fd` must be a valid open file descriptor or -1.
pub unsafe fn close_if_not_std(fd: i32) {
    if fd > 2 {
        // SAFETY: caller guarantees fd is valid or -1
        unsafe { libc::close(fd) };
    }
}

/// Close optional parent-side pipe fds. Used on error paths after fork.
pub fn close_pipe_fds(stdin: Option<i32>, stdout: Option<i32>, stderr: Option<i32>) {
    for fd in [stdin, stdout, stderr].into_iter().flatten() {
        // SAFETY: fd is a valid pipe fd from make_pipe()
        unsafe { libc::close(fd) };
    }
}

/// Read from two optional fds concurrently using `poll`, closing each when done.
/// Avoids deadlock when one pipe buffer fills while we block reading the other.
pub fn read_two_fds(fd1: Option<i32>, fd2: Option<i32>) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf1 = Vec::new();
    let mut buf2 = Vec::new();

    if fd1.is_none() && fd2.is_none() {
        return Ok((buf1, buf2));
    }

    let mut active1 = fd1;
    let mut active2 = fd2;
    let mut tmp = [0u8; 4096];

    'outer: while active1.is_some() || active2.is_some() {
        // Stack-allocated: max 2 entries
        let mut pollfds = [
            libc::pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
            libc::pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
        ];
        let mut fd_buffer_id = [0u8; 2]; // 1 or 2
        let mut nfds = 0usize;

        if let Some(fd) = active1 {
            pollfds[nfds] = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            fd_buffer_id[nfds] = 1;
            nfds += 1;
        }
        if let Some(fd) = active2 {
            pollfds[nfds] = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            fd_buffer_id[nfds] = 2;
            nfds += 1;
        }

        // SAFETY: pollfds is a valid array, -1 means wait indefinitely
        let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), nfds as libc::nfds_t, -1) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            close_pipe_fds(active1, active2, None);
            return Err(err);
        }

        for i in 0..nfds {
            let pfd = &pollfds[i];
            if pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                // SAFETY: fd is valid, tmp buffer is stack-allocated
                let n = unsafe { libc::read(pfd.fd, tmp.as_mut_ptr().cast(), tmp.len()) };
                if n < 0 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EINTR) {
                        continue 'outer;
                    }
                    close_pipe_fds(active1, active2, None);
                    return Err(err);
                }
                if n == 0 {
                    // EOF — close this fd
                    // SAFETY: fd is valid
                    unsafe { libc::close(pfd.fd) };
                    if fd_buffer_id[i] == 1 {
                        active1 = None;
                    } else {
                        active2 = None;
                    }
                } else {
                    let data = &tmp[..n as usize];
                    if fd_buffer_id[i] == 1 {
                        buf1.extend_from_slice(data);
                    } else {
                        buf2.extend_from_slice(data);
                    }
                }
            }
        }
    }

    Ok((buf1, buf2))
}

/// Convert a raw `waitpid` status to `ExitStatus`.
pub fn exit_status_from_raw(status: i32) -> std::process::ExitStatus {
    // SAFETY: from_raw takes a raw wait status on Unix
    std::os::unix::process::ExitStatusExt::from_raw(status)
}

/// Report an error from a forked child and exit. Async-signal-safe: no
/// allocations, only a stack buffer write and `_exit`.
///
/// Writes 8 bytes `[step:i32, errno:i32]` to `err_fd` so the parent can
/// identify which setup step failed.
///
/// # Safety
/// `err_fd` must be a valid writable file descriptor. Must only be called
/// in a forked child process (calls `_exit`).
pub unsafe fn child_bail(err_fd: i32, step: i32, errno: i32) -> ! {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&step.to_ne_bytes());
    buf[4..].copy_from_slice(&errno.to_ne_bytes());
    // SAFETY: err_fd is valid, buf is stack-allocated
    let _ = unsafe { libc::write(err_fd, buf.as_ptr().cast(), 8) };
    // SAFETY: terminates the forked child
    unsafe { libc::_exit(1) }
}

/// Take ownership of an fd from an `Option<i32>` slot, returning it as a `File`.
fn take_fd(slot: &mut Option<i32>) -> Option<std::fs::File> {
    slot.take().map(|fd| {
        // SAFETY: fd is a valid pipe fd we own
        unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) }
    })
}

/// Decode child error report from the error pipe after fork.
/// Returns `Ok(())` if the child started successfully (EOF on pipe),
/// or `Err(SandboxError::Setup(...))` if the child reported a failure.
///
/// On error, reaps the child process and closes parent pipe fds.
///
/// # Safety
/// `err_pipe_rd` must be a valid readable fd. `child_pid` must be a valid PID.
pub unsafe fn check_child_error_pipe(
    err_pipe_rd: i32,
    child_pid: i32,
    parent_stdin: Option<i32>,
    parent_stdout: Option<i32>,
    parent_stderr: Option<i32>,
    step_names: &[&str],
) -> crate::Result<()> {
    let mut err_buf = [0u8; 8];
    let mut bytes_read: usize = 0;
    let mut read_err: Option<io::Error> = None;

    // Loop to accumulate exactly 8 bytes, retrying on EINTR.
    // Pipe writes of <= PIPE_BUF bytes are atomic on POSIX, so short
    // reads are unlikely in practice, but we handle them for correctness.
    loop {
        // SAFETY: err_pipe_rd is valid, writing into remaining portion of err_buf
        let n = unsafe {
            libc::read(
                err_pipe_rd,
                err_buf.as_mut_ptr().add(bytes_read).cast(),
                8 - bytes_read,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            read_err = Some(err);
            break;
        }
        if n == 0 {
            break;
        }
        bytes_read += n as usize;
        if bytes_read >= 8 {
            break;
        }
    }

    // SAFETY: valid fd
    unsafe { libc::close(err_pipe_rd) };

    // Reap child and close parent pipes on any failure path.
    let reap_and_fail = |msg: String| -> crate::SandboxError {
        // SAFETY: valid pid; retry on EINTR to avoid leaving a zombie
        loop {
            let rc = unsafe { libc::waitpid(child_pid, std::ptr::null_mut(), 0) };
            if rc >= 0 {
                break;
            }
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
        close_pipe_fds(parent_stdin, parent_stdout, parent_stderr);
        crate::SandboxError::Setup(msg)
    };

    if let Some(err) = read_err {
        return Err(reap_and_fail(format!(
            "failed to read child error pipe: {err}"
        )));
    }

    if bytes_read == 8 {
        let step = i32::from_ne_bytes([err_buf[0], err_buf[1], err_buf[2], err_buf[3]]);
        let errno = i32::from_ne_bytes([err_buf[4], err_buf[5], err_buf[6], err_buf[7]]);
        let step_name = step_names
            .get((step - 1) as usize)
            .copied()
            .unwrap_or("unknown");
        return Err(reap_and_fail(format!(
            "child setup failed at step '{}': {}",
            step_name,
            io::Error::from_raw_os_error(errno)
        )));
    }

    if bytes_read > 0 {
        return Err(reap_and_fail(format!(
            "child error pipe: incomplete error report ({bytes_read} of 8 bytes)"
        )));
    }

    Ok(())
}

/// How to send SIGKILL to the sandboxed process tree. Linux kills the
/// helper by PID (inner child dies via `PR_SET_PDEATHSIG`). macOS kills
/// the process group (child called `setsid`).
#[allow(dead_code)] // Each variant is only used on one platform
pub enum KillStyle {
    /// `libc::kill(pid, SIGKILL)` — used on Linux where the helper PID
    /// is the target.
    Single,
    /// `libc::killpg(pid, SIGKILL)` — used on macOS where the child
    /// started a new session.
    ProcessGroup,
}

/// Dup2 child fds to stdin/stdout/stderr. Returns `Err(errno)` on failure.
/// Closes original fds if they differ from the target standard fds.
///
/// Handles fd aliasing: if two or more of the input fds share the same
/// underlying fd (e.g., `child_stdout == child_stderr`), earlier dup2+close
/// steps would invalidate the fd before later steps can use it. This is
/// solved by tracking which fds have already been redirected and using the
/// new standard fd number instead of the (now-closed) original.
///
/// # Safety
/// All fds must be valid open descriptors. Must only be called in a
/// forked child before exec.
pub unsafe fn setup_stdio_fds(
    child_stdin: i32,
    child_stdout: i32,
    child_stderr: i32,
) -> std::result::Result<(), i32> {
    // Resolve an input fd through prior dup2 redirections. If a previous
    // step consumed this fd via dup2+close, returns the new standard fd.
    fn effective_fd(fd: i32, redirected: &[(i32, i32); 3], count: usize) -> i32 {
        for &(orig, target) in &redirected[..count] {
            if orig == fd {
                return target;
            }
        }
        fd
    }

    // After dup2+close, the original fd is gone. If another argument
    // aliases it, we must use the already-assigned standard fd instead.
    let mut redirected: [(i32, i32); 3] = [(-1, -1); 3];
    let mut redir_count = 0usize;

    if child_stdin != 0 {
        // SAFETY: caller guarantees child_stdin is a valid open fd
        if unsafe { libc::dup2(child_stdin, 0) } < 0 {
            return Err(errno());
        }
        // SAFETY: fd is valid and > 2 check is inside close_if_not_std
        unsafe { close_if_not_std(child_stdin) };
        redirected[redir_count] = (child_stdin, 0);
        redir_count += 1;
    }

    let stdout_fd = effective_fd(child_stdout, &redirected, redir_count);
    if stdout_fd != 1 {
        // SAFETY: stdout_fd is valid (either the original or a redirected standard fd)
        if unsafe { libc::dup2(stdout_fd, 1) } < 0 {
            return Err(errno());
        }
        // SAFETY: fd is valid; close_if_not_std skips fds 0-2
        unsafe { close_if_not_std(stdout_fd) };
        redirected[redir_count] = (child_stdout, 1);
        redir_count += 1;
    }

    let stderr_fd = effective_fd(child_stderr, &redirected, redir_count);
    if stderr_fd != 2 {
        // SAFETY: stderr_fd is valid (either the original or a redirected standard fd)
        if unsafe { libc::dup2(stderr_fd, 2) } < 0 {
            return Err(errno());
        }
        // SAFETY: fd is valid; close_if_not_std skips fds 0-2
        unsafe { close_if_not_std(stderr_fd) };
    }
    Ok(())
}

/// Set a single resource limit via setrlimit.
///
/// # Safety
/// Must only be called from the forked child before exec.
#[cfg(target_os = "macos")]
unsafe fn set_rlimit(resource: i32, value: u64) -> io::Result<()> {
    let rlim = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: rlim is a valid rlimit struct, caller is in forked child
    if unsafe { libc::setrlimit(resource, &raw const rlim) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Apply resource limits via setrlimit. Used by macOS.
///
/// # Safety
/// Must only be called from the forked child before exec.
#[cfg(target_os = "macos")]
pub unsafe fn apply_resource_limits(policy: &crate::policy::SandboxPolicy) -> io::Result<()> {
    if let Some(max_mem) = policy.limits().max_memory_bytes {
        // SAFETY: caller guarantees forked child context
        unsafe { set_rlimit(libc::RLIMIT_AS, max_mem)? };
    }
    if let Some(max_procs) = policy.limits().max_processes {
        // SAFETY: caller guarantees forked child context
        unsafe { set_rlimit(libc::RLIMIT_NPROC, u64::from(max_procs))? };
    }
    if let Some(max_cpu) = policy.limits().max_cpu_seconds {
        // SAFETY: caller guarantees forked child context
        unsafe { set_rlimit(libc::RLIMIT_CPU, max_cpu)? };
    }
    Ok(())
}

/// Platform-specific errno accessor. Returns the current errno value.
#[allow(unsafe_code)]
pub fn errno() -> i32 {
    #[cfg(target_os = "linux")]
    // SAFETY: errno access has no preconditions
    unsafe {
        *libc::__errno_location()
    }
    #[cfg(not(target_os = "linux"))]
    // SAFETY: errno access has no preconditions
    unsafe {
        *libc::__error()
    }
}

/// Common guard logic for `kill_by_pid`: reject PID 0, negative PIDs,
/// and our own PID. Returns `Some(pid_i32)` if the kill should proceed.
#[cfg(feature = "tokio")]
pub fn validate_kill_pid(pid: u32) -> Option<i32> {
    let pid_i32 = i32::try_from(pid).ok().filter(|&p| p > 0)?;
    if pid == std::process::id() {
        return None;
    }
    Some(pid_i32)
}

/// Generate delegation methods from a platform wrapper to its inner
/// `UnixSandboxedChild`. Platform wrappers hold platform-specific cleanup
/// state so they cannot expose `UnixSandboxedChild` directly; this macro
/// keeps their public API surface identical via delegation.
macro_rules! delegate_unix_child_methods {
    ($field:ident) => {
        pub const fn id(&self) -> u32 {
            self.$field.id()
        }

        pub fn kill(&self) -> std::io::Result<()> {
            self.$field.kill()
        }

        pub fn wait(&self) -> std::io::Result<std::process::ExitStatus> {
            self.$field.wait()
        }

        pub fn try_wait(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
            self.$field.try_wait()
        }

        pub fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
            self.$field.wait_with_output()
        }

        pub fn take_stdin(&mut self) -> Option<std::fs::File> {
            self.$field.take_stdin()
        }

        pub fn take_stdout(&mut self) -> Option<std::fs::File> {
            self.$field.take_stdout()
        }

        pub fn take_stderr(&mut self) -> Option<std::fs::File> {
            self.$field.take_stderr()
        }
    };
}

pub(crate) use delegate_unix_child_methods;

/// Shared lifecycle state for a sandboxed Unix child process.
///
/// Both `LinuxSandboxedChild` and `MacosSandboxedChild` delegate their
/// wait/kill/drop/take_stdio methods here. Platform differences (kill
/// strategy, extra cleanup like cgroup guards) are handled by the
/// platform-specific wrappers.
pub struct UnixSandboxedChild {
    pub(crate) pid: i32,
    pub(crate) stdin_fd: Option<i32>,
    pub(crate) stdout_fd: Option<i32>,
    pub(crate) stderr_fd: Option<i32>,
    /// True once the child has been reaped via waitpid.
    pub(crate) waited: AtomicBool,
    pub(crate) kill_style: KillStyle,
}

impl UnixSandboxedChild {
    pub const fn id(&self) -> u32 {
        self.pid as u32
    }

    /// Send SIGKILL to the child process (or process group), returning the
    /// raw libc return code.
    fn send_sigkill(&self) -> i32 {
        match self.kill_style {
            // SAFETY: valid pid, SIGKILL is a well-known signal
            KillStyle::Single => unsafe { libc::kill(self.pid, libc::SIGKILL) },
            // SAFETY: pid is a valid PGID after setsid(); SIGKILL is well-defined
            KillStyle::ProcessGroup => unsafe { libc::killpg(self.pid, libc::SIGKILL) },
        }
    }

    pub fn kill(&self) -> io::Result<()> {
        // ESRCH from kill() on an already-exited process is treated as success below.
        let rc = self.send_sigkill();
        if rc != 0 {
            let err = io::Error::last_os_error();
            // ESRCH means the process is already gone — not an error.
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        // Atomically claim the reap before calling waitpid to prevent
        // concurrent double-wait racing on the same PID.
        if self
            .waited
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process already waited on",
            ));
        }

        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: valid pid, valid pointer
            let rc = unsafe { libc::waitpid(self.pid, &raw mut status, 0) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
            break;
        }
        Ok(exit_status_from_raw(status))
    }

    pub fn try_wait(&self) -> io::Result<Option<std::process::ExitStatus>> {
        if self
            .waited
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process already waited on",
            ));
        }

        let mut status: libc::c_int = 0;
        // SAFETY: valid pid, valid pointer, WNOHANG for non-blocking
        let rc = unsafe { libc::waitpid(self.pid, &raw mut status, libc::WNOHANG) };
        if rc < 0 {
            self.waited.store(false, Ordering::Release);
            return Err(io::Error::last_os_error());
        }
        if rc == 0 {
            self.waited.store(false, Ordering::Release);
            return Ok(None);
        }
        Ok(Some(exit_status_from_raw(status)))
    }

    pub fn wait_with_output(&mut self) -> io::Result<std::process::Output> {
        // Take fds so Drop won't double-close them
        let stdout_fd = self.stdout_fd.take();
        let stderr_fd = self.stderr_fd.take();

        // Read both pipes concurrently via poll to avoid deadlock when one
        // pipe buffer fills while we block reading the other.
        let (stdout, stderr) = read_two_fds(stdout_fd, stderr_fd)?;
        let status = self.wait()?;

        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }

    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        take_fd(&mut self.stdin_fd)
    }

    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        take_fd(&mut self.stdout_fd)
    }

    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        take_fd(&mut self.stderr_fd)
    }

    /// Close all remaining pipe fds.
    pub fn close_fds(&mut self) {
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

    /// Kill and reap the process. Used by both `kill_and_cleanup`
    /// and `Drop`. Callers must call `close_fds()` first.
    pub fn kill_and_reap(&mut self) {
        // Use CAS to atomically claim the reap, consistent with wait().
        // &mut self prevents concurrent safe calls, but CAS eliminates
        // any theoretical TOCTOU if called via unsafe or interior mutability.
        if self
            .waited
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // Send SIGKILL directly — self.kill() would see waited=true and skip.
            // Ignore errors: we proceed to waitpid regardless.
            let _ = self.send_sigkill();
            loop {
                // SAFETY: valid pid; blocking wait reaps the zombie
                let rc = unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) };
                if rc < 0 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                }
                break;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_wrap)]
mod tests {
    use super::*;

    #[test]
    fn prepare_prefork_basic() {
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.arg("hello");
        cmd.env("FOO", "bar");

        let data = prepare_prefork(&cmd).expect("prepare_prefork");
        assert_eq!(data.program.to_str().unwrap(), "/bin/echo");
        assert_eq!(data.argv.len(), 2); // program + 1 arg
        assert_eq!(data.argv[1].to_str().unwrap(), "hello");
        // argv_ptrs = argv.len() + 1 (null terminator)
        assert_eq!(data.argv_ptrs.len(), 3);
        assert!(data.argv_ptrs.last().unwrap().is_null());
        // envp should contain FOO=bar and a default PATH
        assert!(data.envp.len() >= 2);
    }

    #[test]
    fn prepare_prefork_null_byte_rejected() {
        let cmd = SandboxCommand::new("/bin/\0echo");
        assert!(prepare_prefork(&cmd).is_err());
    }

    #[test]
    fn prepare_prefork_preserves_explicit_path() {
        let mut cmd = SandboxCommand::new("/bin/sh");
        cmd.env("PATH", "/custom/bin");
        let data = prepare_prefork(&cmd).expect("prepare_prefork");
        // Should have exactly one PATH entry (the explicit one), not the default
        let path_entries: Vec<_> = data
            .envp
            .iter()
            .filter(|e| e.to_str().unwrap().starts_with("PATH="))
            .collect();
        assert_eq!(path_entries.len(), 1);
        assert_eq!(path_entries[0].to_str().unwrap(), "PATH=/custom/bin");
    }

    #[test]
    fn prepare_prefork_with_cwd() {
        let mut cmd = SandboxCommand::new("/bin/sh");
        cmd.cwd("/tmp");
        let data = prepare_prefork(&cmd).expect("prepare_prefork");
        assert_eq!(data.cwd.as_ref().unwrap().to_str().unwrap(), "/tmp");
    }

    #[test]
    fn make_pipe_returns_valid_fds() {
        let (r, w) = make_pipe().expect("make_pipe");
        assert!(r >= 0);
        assert!(w >= 0);
        assert_ne!(r, w);
        // Write data before closing so it's buffered in the kernel.
        // FD-number reuse after close(w) is harmless: the pipe's write-end
        // file description is already released, and read sees buffered data + EOF
        // regardless of what the recycled FD number now points to.
        let msg = b"test";
        // SAFETY: w is a valid fd from make_pipe
        let written = unsafe { libc::write(w, msg.as_ptr().cast(), msg.len()) };
        assert_eq!(written, msg.len() as isize);
        // SAFETY: valid fd
        assert_eq!(unsafe { libc::close(w) }, 0, "close write end");
        let mut buf = [0u8; 16];
        // SAFETY: r is a valid fd, buf is stack-allocated
        let n = unsafe { libc::read(r, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(n, msg.len() as isize);
        assert_eq!(&buf[..n as usize], msg);
        // SAFETY: valid fd
        assert_eq!(unsafe { libc::close(r) }, 0, "close read end");
    }

    #[test]
    fn open_dev_null_read() {
        let fd = open_dev_null(libc::O_RDONLY).expect("open /dev/null");
        assert!(fd >= 0);
        // Reading from /dev/null returns EOF immediately
        let mut buf = [0u8; 1];
        // SAFETY: fd is valid, buf is stack-allocated
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), 1) };
        assert_eq!(n, 0);
        // SAFETY: valid fd
        assert_eq!(unsafe { libc::close(fd) }, 0, "close /dev/null");
    }

    #[test]
    fn open_dev_null_write() {
        let fd = open_dev_null(libc::O_WRONLY).expect("open /dev/null");
        assert!(fd >= 0);
        let msg = b"discard";
        // SAFETY: fd is valid, msg is stack-allocated
        let n = unsafe { libc::write(fd, msg.as_ptr().cast(), msg.len()) };
        assert_eq!(n, msg.len() as isize);
        // SAFETY: valid fd
        assert_eq!(unsafe { libc::close(fd) }, 0, "close /dev/null");
    }

    #[test]
    fn read_two_fds_both_none() {
        let (a, b) = read_two_fds(None, None).expect("read_two_fds");
        assert!(a.is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn read_two_fds_one_pipe() {
        let (r, w) = make_pipe().expect("pipe");
        let msg = b"hello";
        // SAFETY: w is a valid fd
        let n = unsafe { libc::write(w, msg.as_ptr().cast(), msg.len()) };
        assert_eq!(n, msg.len() as isize, "write to pipe failed");
        // Data is buffered before close; FD-number reuse after close(w) is
        // harmless -- the pipe's write-end file description is released and
        // read_two_fds sees buffered data + EOF regardless.
        // SAFETY: valid fd
        assert_eq!(unsafe { libc::close(w) }, 0, "close write end");

        let (data, empty) = read_two_fds(Some(r), None).expect("read_two_fds");
        assert_eq!(data, b"hello");
        assert!(empty.is_empty());
    }

    #[test]
    fn read_two_fds_two_pipes() {
        let (r1, w1) = make_pipe().expect("pipe1");
        let (r2, w2) = make_pipe().expect("pipe2");
        // Write all data before closing so it's buffered in the kernel.
        // FD-number reuse after close is harmless (see make_pipe_returns_valid_fds).
        // SAFETY: valid fds
        let n1 = unsafe { libc::write(w1, b"aaa".as_ptr().cast(), 3) };
        assert_eq!(n1, 3, "write to pipe1 failed");
        let n2 = unsafe { libc::write(w2, b"bbb".as_ptr().cast(), 3) };
        assert_eq!(n2, 3, "write to pipe2 failed");
        assert_eq!(unsafe { libc::close(w1) }, 0, "close w1");
        assert_eq!(unsafe { libc::close(w2) }, 0, "close w2");

        let (d1, d2) = read_two_fds(Some(r1), Some(r2)).expect("read_two_fds");
        assert_eq!(d1, b"aaa");
        assert_eq!(d2, b"bbb");
    }

    #[test]
    fn exit_status_from_raw_zero_is_success() {
        let status = exit_status_from_raw(0);
        assert!(status.success());
    }

    #[test]
    fn exit_status_from_raw_signal() {
        // Raw status for killed by SIGKILL (9): signal number in low byte
        let status = exit_status_from_raw(9);
        assert!(!status.success());
    }

    #[test]
    fn close_if_not_std_skips_standard_fds() {
        // SAFETY: Should not close stdin/stdout/stderr
        unsafe {
            close_if_not_std(0);
            close_if_not_std(1);
            close_if_not_std(2);
        }
        // If we got here without crashing, the test passes
    }

    #[test]
    fn close_if_not_std_closes_non_standard_fd() {
        let (read_fd, write_fd) = make_pipe().expect("make_pipe");
        assert!(read_fd > 2);

        // SAFETY: read_fd is a valid open fd from make_pipe()
        unsafe { close_if_not_std(read_fd) };

        // Verify close: F_GETFD on a closed FD returns -1/EBADF.
        // SAFETY: fcntl on any integer fd is defined (returns EBADF if invalid).
        let ret = unsafe { libc::fcntl(read_fd, libc::F_GETFD) };
        if ret == -1 {
            let err = std::io::Error::last_os_error();
            assert_eq!(
                err.raw_os_error(),
                Some(libc::EBADF),
                "expected EBADF on closed fd"
            );
        } else {
            // FD number was recycled between close and fcntl. Fall back to
            // EPIPE check: FD-number reuse doesn't reopen the pipe's read end,
            // so write must still fail with EPIPE if close_if_not_std worked.
            // SAFETY: SIGPIPE must be ignored to get EPIPE instead of a signal.
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
            // SAFETY: write_fd is a valid open fd from make_pipe()
            let w = unsafe { libc::write(write_fd, b"x".as_ptr().cast(), 1) };
            let werr = std::io::Error::last_os_error();
            assert_eq!(w, -1, "write should fail (EPIPE) after read end closed");
            assert_eq!(
                werr.raw_os_error(),
                Some(libc::EPIPE),
                "expected EPIPE after closing read end"
            );
        }

        // SAFETY: write_fd is still open and needs closing to avoid leak
        assert_eq!(unsafe { libc::close(write_fd) }, 0, "close write_fd");
    }

    #[test]
    fn setup_stdio_pipes_all_piped() {
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.stdin(SandboxStdio::Piped);
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let pipes = setup_stdio_pipes(&cmd).expect("setup_stdio_pipes");
        assert!(pipes.child_stdin > 2);
        assert!(pipes.child_stdout > 2);
        assert!(pipes.child_stderr > 2);
        assert!(pipes.parent_stdin.is_some());
        assert!(pipes.parent_stdout.is_some());
        assert!(pipes.parent_stderr.is_some());

        // Clean up fds
        // SAFETY: all fds are valid pipe fds from setup_stdio_pipes
        unsafe {
            assert_eq!(libc::close(pipes.child_stdin), 0, "close child_stdin");
            assert_eq!(libc::close(pipes.child_stdout), 0, "close child_stdout");
            assert_eq!(libc::close(pipes.child_stderr), 0, "close child_stderr");
            assert_eq!(
                libc::close(pipes.parent_stdin.unwrap()),
                0,
                "close parent_stdin"
            );
            assert_eq!(
                libc::close(pipes.parent_stdout.unwrap()),
                0,
                "close parent_stdout"
            );
            assert_eq!(
                libc::close(pipes.parent_stderr.unwrap()),
                0,
                "close parent_stderr"
            );
        }
    }

    #[test]
    fn setup_stdio_pipes_all_null() {
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.stdin(SandboxStdio::Null);
        cmd.stdout(SandboxStdio::Null);
        cmd.stderr(SandboxStdio::Null);

        let pipes = setup_stdio_pipes(&cmd).expect("setup_stdio_pipes");
        assert!(pipes.child_stdin > 2);
        assert!(pipes.child_stdout > 2);
        assert!(pipes.child_stderr > 2);
        assert!(pipes.parent_stdin.is_none());
        assert!(pipes.parent_stdout.is_none());
        assert!(pipes.parent_stderr.is_none());

        // Clean up fds
        // SAFETY: fds are valid from setup_stdio_pipes
        unsafe {
            assert_eq!(libc::close(pipes.child_stdin), 0, "close child_stdin");
            assert_eq!(libc::close(pipes.child_stdout), 0, "close child_stdout");
            assert_eq!(libc::close(pipes.child_stderr), 0, "close child_stderr");
        }
    }

    #[test]
    fn setup_stdio_pipes_inherit() {
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.stdin(SandboxStdio::Inherit);
        cmd.stdout(SandboxStdio::Inherit);
        cmd.stderr(SandboxStdio::Inherit);

        let pipes = setup_stdio_pipes(&cmd).expect("setup_stdio_pipes");
        assert_eq!(pipes.child_stdin, 0);
        assert_eq!(pipes.child_stdout, 1);
        assert_eq!(pipes.child_stderr, 2);
        assert!(pipes.parent_stdin.is_none());
        assert!(pipes.parent_stdout.is_none());
        assert!(pipes.parent_stderr.is_none());
        // No fds to clean up -- these are stdin/stdout/stderr
    }

    // ── check_child_error_pipe tests ─────────────────────────────────

    /// Helper: fork a child that writes `data` to a pipe, then exits.
    /// Returns the read end of the pipe and the child PID.
    fn fork_pipe_writer(data: &[u8]) -> (i32, i32) {
        let (r, w) = make_pipe().expect("make_pipe");
        // SAFETY: fork + write + _exit are async-signal-safe
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // Child: close read end, write data, exit
            unsafe {
                libc::close(r);
                if !data.is_empty() {
                    libc::write(w, data.as_ptr().cast(), data.len());
                }
                libc::close(w);
                libc::_exit(0);
            }
        }
        // Parent: close write end
        assert_eq!(unsafe { libc::close(w) }, 0, "close write end in parent");
        (r, pid)
    }

    #[test]
    fn check_child_error_pipe_empty_pipe_succeeds() {
        // Empty pipe (EOF) means child started successfully.
        let (r, pid) = fork_pipe_writer(b"");
        let step_names = &["step1", "step2"];
        // SAFETY: r and pid are valid from fork_pipe_writer
        let result = unsafe { check_child_error_pipe(r, pid, None, None, None, step_names) };
        assert!(
            result.is_ok(),
            "empty pipe should indicate success: {result:?}"
        );
        // Reap the child to avoid zombie.
        assert!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) } > 0,
            "waitpid"
        );
    }

    #[test]
    fn check_child_error_pipe_with_error_data() {
        // Write 8 bytes: step=1 (i32), errno=2 (ENOENT on most systems)
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&1_i32.to_ne_bytes());
        data[4..].copy_from_slice(&2_i32.to_ne_bytes());

        let (r, pid) = fork_pipe_writer(&data);
        let step_names = &["execve", "chdir"];
        // SAFETY: r and pid are valid
        let result = unsafe { check_child_error_pipe(r, pid, None, None, None, step_names) };
        assert!(
            result.is_err(),
            "8 bytes of error data should produce an error"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("execve"),
            "error should mention the step name: {msg}"
        );
    }

    #[test]
    fn check_child_error_pipe_with_partial_data() {
        // Write fewer than 8 bytes -- should report incomplete error.
        let partial = [0u8; 5];
        let (r, pid) = fork_pipe_writer(&partial);
        let step_names = &["step1"];
        // SAFETY: r and pid are valid
        let result = unsafe { check_child_error_pipe(r, pid, None, None, None, step_names) };
        assert!(result.is_err(), "partial data should produce an error");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("incomplete"),
            "error should mention incomplete: {msg}"
        );
    }

    #[test]
    fn check_child_error_pipe_unknown_step() {
        // Step index out of range should produce "unknown" step name.
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&99_i32.to_ne_bytes());
        data[4..].copy_from_slice(&1_i32.to_ne_bytes());

        let (r, pid) = fork_pipe_writer(&data);
        let step_names = &["only_one"];
        // SAFETY: r and pid are valid
        let result = unsafe { check_child_error_pipe(r, pid, None, None, None, step_names) };
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("unknown"),
            "out-of-range step should be 'unknown': {msg}"
        );
    }

    #[test]
    fn check_child_error_pipe_closes_parent_pipes_on_error() {
        let mut data = [0u8; 8];
        data[..4].copy_from_slice(&1_i32.to_ne_bytes());
        data[4..].copy_from_slice(&1_i32.to_ne_bytes());

        let (r, pid) = fork_pipe_writer(&data);

        // Two separate pipes: pass one end of each as parent_stdin / parent_stdout.
        // Keep the partner ends to verify closure via pipe behavior.
        let (witness_r, passed_w) = make_pipe().expect("pipe for stdin witness");
        let (passed_r, witness_w) = make_pipe().expect("pipe for stdout witness");

        // SAFETY: all fds and pid are valid
        let result = unsafe {
            check_child_error_pipe(r, pid, Some(passed_w), Some(passed_r), None, &["step"])
        };
        assert!(result.is_err());

        // Verify passed_w (write end) was closed: reading from witness_r returns EOF.
        // SAFETY: witness_r is a valid fd from make_pipe()
        let mut buf = [0u8; 1];
        let ret = unsafe { libc::read(witness_r, buf.as_mut_ptr().cast(), 1) };
        assert_eq!(ret, 0, "expected EOF — write end should have been closed");
        // SAFETY: witness_r is a valid fd
        assert_eq!(unsafe { libc::close(witness_r) }, 0, "close witness_r");

        // Verify passed_r (read end) was closed: writing to witness_w produces EPIPE.
        // SAFETY: witness_w is a valid fd; SIGPIPE ignored process-wide in tests.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        let ret = unsafe { libc::write(witness_w, b"x".as_ptr().cast(), 1) };
        let err = std::io::Error::last_os_error();
        assert_eq!(ret, -1, "write should fail when read end is closed");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EPIPE),
            "expected EPIPE after closing read end"
        );
        // SAFETY: witness_w is a valid fd
        assert_eq!(unsafe { libc::close(witness_w) }, 0, "close witness_w");
    }

    // ── validate_kill_pid tests ──────────────────────────────────────

    #[cfg(feature = "tokio")]
    #[test]
    fn validate_kill_pid_rejects_zero() {
        assert!(validate_kill_pid(0).is_none());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn validate_kill_pid_rejects_own_pid() {
        assert!(validate_kill_pid(std::process::id()).is_none());
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn validate_kill_pid_accepts_valid_pid() {
        // Use PID 1 (init) as a known valid PID (we won't actually kill it).
        let result = validate_kill_pid(1);
        assert_eq!(result, Some(1));
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn validate_kill_pid_rejects_overflow() {
        // u32::MAX cannot fit in i32 as a positive value.
        assert!(validate_kill_pid(u32::MAX).is_none());
    }

    // ── UnixSandboxedChild tests ─────────────────────────────────────

    /// Create a UnixSandboxedChild from a real forked child that exits immediately.
    /// Returns the child and a sync fd: reading from the fd blocks until the child exits.
    fn spawn_trivial_child() -> (UnixSandboxedChild, i32) {
        let (sync_r, sync_w) = make_pipe().expect("sync pipe");
        // SAFETY: fork + _exit. The child closes sync_w implicitly via _exit,
        // causing EOF on sync_r in the parent.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                libc::close(sync_r);
                // sync_w closes when the child exits
                libc::_exit(0);
            }
        }
        // SAFETY: parent closes write-end so only the child holds it.
        assert_eq!(unsafe { libc::close(sync_w) }, 0, "close sync_w");
        let child = UnixSandboxedChild {
            pid,
            stdin_fd: None,
            stdout_fd: None,
            stderr_fd: None,
            waited: AtomicBool::new(false),
            kill_style: KillStyle::Single,
        };
        (child, sync_r)
    }

    /// Block until the child exits by reading from the sync pipe fd, then close it.
    fn wait_for_child_exit(sync_fd: i32) {
        let mut buf = [0u8; 1];
        // SAFETY: reading from a valid pipe fd; returns 0 on EOF (child exited).
        let n = unsafe { libc::read(sync_fd, buf.as_mut_ptr().cast::<libc::c_void>(), 1) };
        assert_eq!(n, 0, "expected EOF from sync pipe");
        assert_eq!(unsafe { libc::close(sync_fd) }, 0, "close sync_fd");
    }

    #[test]
    fn unix_child_id_returns_pid() {
        let (child, sync_fd) = spawn_trivial_child();
        assert!(child.id() > 0);
        wait_for_child_exit(sync_fd);
        // Reap to avoid zombie
        let _ = child.wait();
    }

    #[test]
    fn unix_child_wait_returns_success() {
        let (child, sync_fd) = spawn_trivial_child();
        wait_for_child_exit(sync_fd);
        let status = child.wait().expect("wait");
        assert!(status.success());
    }

    #[test]
    fn unix_child_double_wait_fails() {
        let (child, sync_fd) = spawn_trivial_child();
        wait_for_child_exit(sync_fd);
        let _ = child.wait().expect("first wait");
        let result = child.wait();
        assert!(result.is_err(), "second wait should fail");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("already waited"),
            "error should mention already waited: {msg}"
        );
    }

    #[test]
    fn unix_child_try_wait_returns_result() {
        let (child, sync_fd) = spawn_trivial_child();
        // Wait for child to exit via pipe EOF.
        wait_for_child_exit(sync_fd);
        // Pipe EOF guarantees _exit() was called, but on macOS there is a
        // window before the kernel makes the child waitable via waitpid(WNOHANG).
        // Retry briefly instead of assuming immediate availability.
        let mut result = None;
        for _ in 0..50 {
            match child.try_wait().expect("try_wait") {
                Some(status) => {
                    result = Some(status);
                    break;
                }
                None => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let status = result.expect("child should have exited within 500ms");
        assert!(status.success());
    }

    #[test]
    fn unix_child_kill_already_exited() {
        let (child, sync_fd) = spawn_trivial_child();
        // Wait for child to exit via pipe EOF.
        wait_for_child_exit(sync_fd);
        // kill() should not fail even if the child already exited.
        let kill_result = child.kill();
        assert!(
            kill_result.is_ok(),
            "kill on exited child should succeed: {kill_result:?}"
        );
        // Reap to avoid zombie.
        let _ = child.wait();
    }

    #[test]
    fn unix_child_kill_and_reap() {
        // Use a pipe so the child blocks without a long sleep timeout.
        let (read_end, write_end) = make_pipe().expect("pipe");
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            // SAFETY: child process — close write end, block on read until killed.
            unsafe {
                libc::close(write_end);
                let mut buf = [0u8; 1];
                libc::read(read_end, buf.as_mut_ptr().cast(), 1);
                libc::_exit(0);
            }
        }
        // Parent: close read end; keep write end so child stays blocked.
        assert_eq!(unsafe { libc::close(read_end) }, 0, "close read_end");
        let mut child = UnixSandboxedChild {
            pid,
            stdin_fd: None,
            stdout_fd: None,
            stderr_fd: None,
            waited: AtomicBool::new(false),
            kill_style: KillStyle::Single,
        };
        child.kill_and_reap();
        assert_eq!(unsafe { libc::close(write_end) }, 0, "close write_end");
        // After kill_and_reap, waited should be true.
        assert!(child.waited.load(Ordering::Acquire));
    }

    #[test]
    fn unix_child_close_fds() {
        // Three separate pipes so each fd has a witness partner for
        // race-free closure verification via pipe behavior.
        let (witness_stdin, fd_stdin) = make_pipe().expect("pipe");
        let (fd_stdout, witness_stdout) = make_pipe().expect("pipe");
        let (fd_stderr, witness_stderr) = make_pipe().expect("pipe");

        let mut child2 = UnixSandboxedChild {
            pid: 1,
            stdin_fd: Some(fd_stdin),
            stdout_fd: Some(fd_stdout),
            stderr_fd: Some(fd_stderr),
            waited: AtomicBool::new(true),
            kill_style: KillStyle::Single,
        };

        child2.close_fds();
        assert!(child2.stdin_fd.is_none());
        assert!(child2.stdout_fd.is_none());
        assert!(child2.stderr_fd.is_none());

        // Verify fd_stdin (write end) was closed: read from witness returns EOF.
        // SAFETY: witness_stdin is a valid fd from make_pipe()
        let mut buf = [0u8; 1];
        let ret = unsafe { libc::read(witness_stdin, buf.as_mut_ptr().cast(), 1) };
        assert_eq!(ret, 0, "expected EOF — stdin write end should be closed");
        assert_eq!(
            unsafe { libc::close(witness_stdin) },
            0,
            "close witness_stdin"
        );

        // Verify fd_stdout (read end) was closed: write to witness produces EPIPE.
        // SAFETY: witness_stdout is a valid fd; SIGPIPE ignored process-wide in tests.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        let ret = unsafe { libc::write(witness_stdout, b"x".as_ptr().cast(), 1) };
        let err = std::io::Error::last_os_error();
        assert_eq!(ret, -1, "write should fail when read end is closed");
        assert_eq!(err.raw_os_error(), Some(libc::EPIPE), "expected EPIPE");
        assert_eq!(
            unsafe { libc::close(witness_stdout) },
            0,
            "close witness_stdout"
        );

        // Verify fd_stderr (read end) was closed: write to witness produces EPIPE.
        let ret = unsafe { libc::write(witness_stderr, b"x".as_ptr().cast(), 1) };
        let err = std::io::Error::last_os_error();
        assert_eq!(ret, -1, "write should fail when read end is closed");
        assert_eq!(err.raw_os_error(), Some(libc::EPIPE), "expected EPIPE");
        assert_eq!(
            unsafe { libc::close(witness_stderr) },
            0,
            "close witness_stderr"
        );
    }

    #[test]
    fn unix_child_take_stdin_stdout_stderr() {
        let (r, w) = make_pipe().expect("pipe");
        let mut child2 = UnixSandboxedChild {
            pid: 1,
            stdin_fd: Some(w),
            stdout_fd: Some(r),
            stderr_fd: None,
            waited: AtomicBool::new(true),
            kill_style: KillStyle::Single,
        };

        let stdin_file = child2.take_stdin();
        assert!(stdin_file.is_some());
        assert!(child2.stdin_fd.is_none());
        // Second take should return None.
        assert!(child2.take_stdin().is_none());

        let stdout_file = child2.take_stdout();
        assert!(stdout_file.is_some());
        assert!(child2.take_stderr().is_none());

        // Drop files to close fds.
        drop(stdin_file);
        drop(stdout_file);
    }

    #[test]
    fn unix_child_wait_with_output() {
        // Fork a child that writes to stdout/stderr pipes, then exits.
        let (stdout_r, stdout_w) = make_pipe().expect("stdout pipe");
        let (stderr_r, stderr_w) = make_pipe().expect("stderr pipe");

        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(stdout_r);
                libc::close(stderr_r);
                libc::write(stdout_w, b"out".as_ptr().cast(), 3);
                libc::write(stderr_w, b"err".as_ptr().cast(), 3);
                libc::close(stdout_w);
                libc::close(stderr_w);
                libc::_exit(0);
            }
        }
        unsafe {
            assert_eq!(libc::close(stdout_w), 0, "close stdout_w");
            assert_eq!(libc::close(stderr_w), 0, "close stderr_w");
        }

        let mut child = UnixSandboxedChild {
            pid,
            stdin_fd: None,
            stdout_fd: Some(stdout_r),
            stderr_fd: Some(stderr_r),
            waited: AtomicBool::new(false),
            kill_style: KillStyle::Single,
        };

        let output = child.wait_with_output().expect("wait_with_output");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn set_rlimit_nofile_succeeds() {
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: querying current RLIMIT_NOFILE into a valid rlimit struct
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut current) };
        // SAFETY: setting limit to current soft limit (effectively a no-op)
        let result = unsafe { set_rlimit(libc::RLIMIT_NOFILE, current.rlim_cur) };
        assert!(
            result.is_ok(),
            "set_rlimit to current soft limit should succeed: {result:?}"
        );
    }
}
