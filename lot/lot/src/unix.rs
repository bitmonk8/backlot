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
    let mut env_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (k, v) in &command.env {
        env_pairs.push((k.as_bytes().to_vec(), v.as_bytes().to_vec()));
    }
    let has_path = env_pairs.iter().any(|(k, _)| k == b"PATH");
    if !has_path {
        env_pairs.push((
            b"PATH".to_vec(),
            b"/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_vec(),
        ));
    }

    let envp: Vec<CString> = env_pairs
        .iter()
        .map(|(k, v)| {
            let mut entry = k.clone();
            entry.push(b'=');
            entry.extend_from_slice(v);
            CString::new(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
        })
        .collect::<io::Result<_>>()?;

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
/// Uses `pipe2` on Linux (atomic), `pipe` + `fcntl` on macOS.
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
        // Set CLOEXEC on both ends so they don't leak to exec'd process
        for &fd in &fds {
            // SAFETY: fd is valid from pipe()
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags >= 0 {
                // SAFETY: fd is valid, setting FD_CLOEXEC
                unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
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
pub fn close_parent_pipes(stdin: Option<i32>, stdout: Option<i32>, stderr: Option<i32>) {
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
            close_parent_pipes(active1, active2, None);
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
                    close_parent_pipes(active1, active2, None);
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

/// How to send SIGKILL to the sandboxed process tree. Linux kills the
/// helper by PID (inner child dies via `PR_SET_PDEATHSIG`). macOS kills
/// the process group (child called `setsid`).
#[allow(dead_code)] // Each variant is only used on one platform
pub enum KillStyle {
    /// `libc::kill(pid, SIGKILL)` — used on Linux where the helper PID
    /// is the target.
    Kill,
    /// `libc::killpg(pid, SIGKILL)` — used on macOS where the child
    /// started a new session.
    KillProcessGroup,
}

/// Shared lifecycle state for a sandboxed Unix child process.
///
/// Both `LinuxSandboxedChild` and `MacSandboxedChild` delegate their
/// wait/kill/drop/take_stdio methods here. Platform differences (kill
/// strategy, extra cleanup like cgroup guards) are handled by the
/// platform-specific wrappers.
pub struct UnixSandboxedChild {
    pub pid: i32,
    pub stdin_fd: Option<i32>,
    pub stdout_fd: Option<i32>,
    pub stderr_fd: Option<i32>,
    /// True once the child has been reaped via waitpid.
    pub waited: AtomicBool,
    pub kill_style: KillStyle,
}

impl UnixSandboxedChild {
    pub const fn id(&self) -> u32 {
        self.pid as u32
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if self.waited.load(Ordering::Acquire) {
            return Ok(());
        }
        let rc = match self.kill_style {
            // SAFETY: valid pid, SIGKILL is a well-known signal
            KillStyle::Kill => unsafe { libc::kill(self.pid, libc::SIGKILL) },
            // SAFETY: pid is a valid PGID after setsid(); SIGKILL is well-defined
            KillStyle::KillProcessGroup => unsafe { libc::killpg(self.pid, libc::SIGKILL) },
        };
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
        // Atomically claim the reap BEFORE calling waitpid so a concurrent
        // wait() cannot race and get ECHILD.
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
            // Revert the claim — waitpid failed, child is not reaped.
            self.waited.store(false, Ordering::Release);
            return Err(io::Error::last_os_error());
        }
        if rc == 0 {
            // Child still running — revert the claim.
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
        if !self.waited.load(Ordering::Acquire) {
            let _ = self.kill();
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
            self.waited.store(true, Ordering::Release);
        }
    }
}
