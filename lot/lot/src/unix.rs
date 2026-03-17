//! Shared infrastructure for Unix backends (Linux, macOS).
//!
//! Contains pipe creation, stdio setup, concurrent pipe reading, fd helpers,
//! and `ExitStatus` conversion that are identical across Linux and macOS.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;

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

/// Set up stdio for the child process. Creates pipes as needed.
///
/// Returns `(child_stdin_rd, child_stdout_wr, child_stderr_wr,
///           parent_stdin_wr, parent_stdout_rd, parent_stderr_rd)`.
pub fn setup_stdio_pipes(
    command: &SandboxCommand,
) -> io::Result<(i32, i32, i32, Option<i32>, Option<i32>, Option<i32>)> {
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
            let (r, w) = make_pipe().map_err(|e| {
                // Clean up stdin fds before propagating error
                // SAFETY: fds are valid from the successful stdin step above
                unsafe { close_if_not_std(child_stdin) };
                close_parent_pipes(parent_stdin, None, None);
                e
            })?;
            (w, Some(r))
        }
        SandboxStdio::Null => (open_dev_null(libc::O_WRONLY).map_err(|e| {
            unsafe { close_if_not_std(child_stdin) };
            close_parent_pipes(parent_stdin, None, None);
            e
        })?, None),
        SandboxStdio::Inherit => (1, None),
    };

    let (child_stderr, parent_stderr) = match command.stderr {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe().map_err(|e| {
                // Clean up stdin+stdout fds before propagating error
                // SAFETY: fds are valid from successful steps above
                unsafe { close_if_not_std(child_stdin) };
                unsafe { close_if_not_std(child_stdout) };
                close_parent_pipes(parent_stdin, parent_stdout, None);
                e
            })?;
            (w, Some(r))
        }
        SandboxStdio::Null => (open_dev_null(libc::O_WRONLY).map_err(|e| {
            unsafe { close_if_not_std(child_stdin) };
            unsafe { close_if_not_std(child_stdout) };
            close_parent_pipes(parent_stdin, parent_stdout, None);
            e
        })?, None),
        SandboxStdio::Inherit => (2, None),
    };

    Ok((
        child_stdin,
        child_stdout,
        child_stderr,
        parent_stdin,
        parent_stdout,
        parent_stderr,
    ))
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
        let mut idx_map = [0u8; 2]; // 1 or 2
        let mut nfds = 0usize;

        if let Some(fd) = active1 {
            pollfds[nfds] = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            idx_map[nfds] = 1;
            nfds += 1;
        }
        if let Some(fd) = active2 {
            pollfds[nfds] = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            idx_map[nfds] = 2;
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
                    if idx_map[i] == 1 {
                        active1 = None;
                    } else {
                        active2 = None;
                    }
                } else {
                    let data = &tmp[..n as usize];
                    if idx_map[i] == 1 {
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
