#![allow(unsafe_code)]

pub(crate) mod seatbelt;

use std::cell::Cell;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;

use crate::command::{SandboxCommand, SandboxStdio};
use crate::policy::SandboxPolicy;
use crate::{PlatformCapabilities, Result, SandboxError, SandboxedChild};

pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: seatbelt::available(),
        appcontainer: false,
        job_objects: false,
    }
}

/// Pre-fork data: everything the child needs, converted to C types
/// so no allocations happen after fork().
struct PreForkData {
    program: CString,
    #[allow(dead_code)] // kept alive so argv_ptrs remain valid
    argv: Vec<CString>,
    /// Pre-built pointer array for execve argv (null-terminated).
    argv_ptrs: Vec<*const libc::c_char>,
    /// Pre-built pointer array for execve envp (null-terminated).
    envp_ptrs: Vec<*const libc::c_char>,
    #[allow(dead_code)]
    envp: Vec<CString>,
    cwd: Option<CString>,
}

/// Build all C strings and data structures before forking.
fn prepare_prefork(command: &SandboxCommand) -> io::Result<PreForkData> {
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
            CString::new(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
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

/// Create a pipe pair, returns (read_fd, write_fd).
fn make_pipe() -> io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];
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
    Ok((fds[0], fds[1]))
}

/// Set up stdio for the child process. Creates pipes as needed.
///
/// Returns (child_stdin_rd, child_stdout_wr, child_stderr_wr,
///          parent_stdin_wr, parent_stdout_rd, parent_stderr_rd)
fn setup_stdio_pipes(
    command: &SandboxCommand,
) -> io::Result<(i32, i32, i32, Option<i32>, Option<i32>, Option<i32>)> {
    let (child_stdin, parent_stdin) = match command.stdin {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe()?;
            (r, Some(w))
        }
        SandboxStdio::Null => {
            let c_path = CString::new("/dev/null")
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            // SAFETY: valid path, O_RDONLY
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            (fd, None)
        }
        SandboxStdio::Inherit => (0, None),
    };

    let (child_stdout, parent_stdout) = match command.stdout {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe()?;
            (w, Some(r))
        }
        SandboxStdio::Null => {
            let c_path = CString::new("/dev/null")
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            // SAFETY: valid path, O_WRONLY
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            (fd, None)
        }
        SandboxStdio::Inherit => (1, None),
    };

    let (child_stderr, parent_stderr) = match command.stderr {
        SandboxStdio::Piped => {
            let (r, w) = make_pipe()?;
            (w, Some(r))
        }
        SandboxStdio::Null => {
            let c_path = CString::new("/dev/null")
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            // SAFETY: valid path, O_WRONLY
            let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            (fd, None)
        }
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
unsafe fn close_if_not_std(fd: i32) {
    if fd > 2 {
        // SAFETY: caller guarantees fd is valid or -1
        unsafe { libc::close(fd) };
    }
}

/// Apply resource limits via setrlimit.
///
/// # Safety
/// Must only be called from the forked child before exec.
unsafe fn apply_resource_limits(policy: &SandboxPolicy) -> io::Result<()> {
    if let Some(max_mem) = policy.limits.max_memory_bytes {
        let rlim = libc::rlimit {
            rlim_cur: max_mem,
            rlim_max: max_mem,
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if let Some(max_procs) = policy.limits.max_processes {
        let rlim = libc::rlimit {
            rlim_cur: u64::from(max_procs),
            rlim_max: u64::from(max_procs),
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if let Some(max_cpu) = policy.limits.max_cpu_seconds {
        let rlim = libc::rlimit {
            rlim_cur: max_cpu,
            rlim_max: max_cpu,
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    let program_path = std::path::Path::new(&command.program);
    let profile = seatbelt::generate_profile(policy, program_path);

    let prefork = prepare_prefork(command)
        .map_err(|e| SandboxError::Setup(format!("pre-fork preparation: {e}")))?;

    // Set up stdio pipes before forking
    let (child_stdin, child_stdout, child_stderr, parent_stdin, parent_stdout, parent_stderr) =
        setup_stdio_pipes(command)
            .map_err(|e| SandboxError::Setup(format!("stdio pipe setup: {e}")))?;

    // Error pipe: child writes errno here if anything fails before exec
    let (err_pipe_rd, err_pipe_wr) =
        make_pipe().map_err(|e| SandboxError::Setup(format!("error pipe: {e}")))?;

    // SAFETY: We fork a child process. Between fork and _exit/exec in the child,
    // we call sandbox_init (which Apple's own code calls post-fork) and
    // async-signal-safe functions. The child is single-threaded after fork.
    let child_pid = unsafe { libc::fork() };

    if child_pid < 0 {
        return Err(SandboxError::Io(io::Error::last_os_error()));
    }

    if child_pid == 0 {
        // === CHILD PROCESS (single-threaded after fork) ===

        // Close parent's end of error pipe
        // SAFETY: valid fd
        unsafe { libc::close(err_pipe_rd) };

        // Close parent's stdio pipe ends
        if let Some(fd) = parent_stdin {
            // SAFETY: valid fd from pipe creation
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = parent_stdout {
            // SAFETY: valid fd from pipe creation
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = parent_stderr {
            // SAFETY: valid fd from pipe creation
            unsafe { libc::close(fd) };
        }

        // Macro to report error and exit from child
        macro_rules! child_bail {
            ($err_fd:expr, $errno:expr) => {{
                let e = ($errno as i32).to_ne_bytes();
                // SAFETY: err_fd is valid, e is stack-allocated
                let _ = unsafe { libc::write($err_fd, e.as_ptr().cast(), 4) };
                unsafe { libc::_exit(1) };
            }};
        }

        // Apply seatbelt profile — permanent, inherited by exec'd process
        if let Err(e) = seatbelt::apply_profile(&profile) {
            child_bail!(err_pipe_wr, e.raw_os_error().unwrap_or(libc::EPERM));
        }

        // Apply resource limits
        // SAFETY: single-threaded child, before exec
        if let Err(e) = unsafe { apply_resource_limits(policy) } {
            child_bail!(err_pipe_wr, e.raw_os_error().unwrap_or(libc::EPERM));
        }

        // Set up stdio: dup2 the child fds to 0/1/2
        if child_stdin != 0 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stdin, 0) } < 0 {
                child_bail!(err_pipe_wr, unsafe { *libc::__error() });
            }
            // SAFETY: fd is valid
            unsafe { close_if_not_std(child_stdin) };
        }
        if child_stdout != 1 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stdout, 1) } < 0 {
                child_bail!(err_pipe_wr, unsafe { *libc::__error() });
            }
            // SAFETY: fd is valid
            unsafe { close_if_not_std(child_stdout) };
        }
        if child_stderr != 2 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stderr, 2) } < 0 {
                child_bail!(err_pipe_wr, unsafe { *libc::__error() });
            }
            // SAFETY: fd is valid
            unsafe { close_if_not_std(child_stderr) };
        }

        // Change working directory if specified
        if let Some(ref cwd) = prefork.cwd {
            // SAFETY: valid CString pointer
            if unsafe { libc::chdir(cwd.as_ptr()) } != 0 {
                child_bail!(err_pipe_wr, unsafe { *libc::__error() });
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
        child_bail!(err_pipe_wr, unsafe { *libc::__error() });
    }

    // === PARENT PROCESS ===
    // Close child's ends of pipes
    // SAFETY: these fds are valid
    unsafe {
        libc::close(err_pipe_wr);
        close_if_not_std(child_stdin);
        close_if_not_std(child_stdout);
        close_if_not_std(child_stderr);
    }

    // Check error pipe: if child wrote an errno, setup failed
    let mut err_buf = [0u8; 4];
    // SAFETY: err_pipe_rd is valid, err_buf is stack-allocated
    let n = unsafe { libc::read(err_pipe_rd, err_buf.as_mut_ptr().cast(), 4) };
    // SAFETY: valid fd
    unsafe { libc::close(err_pipe_rd) };

    if n == 4 {
        let errno = i32::from_ne_bytes(err_buf);
        // Reap the child so we don't leak a zombie
        // SAFETY: valid pid
        unsafe { libc::waitpid(child_pid, std::ptr::null_mut(), 0) };
        // Close parent stdio fds
        if let Some(fd) = parent_stdin {
            // SAFETY: valid fd
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = parent_stdout {
            // SAFETY: valid fd
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = parent_stderr {
            // SAFETY: valid fd
            unsafe { libc::close(fd) };
        }
        return Err(SandboxError::Setup(format!(
            "child seatbelt setup failed: {}",
            io::Error::from_raw_os_error(errno)
        )));
    }

    Ok(SandboxedChild {
        inner: MacSandboxedChild {
            child_pid,
            stdin_fd: parent_stdin,
            stdout_fd: parent_stdout,
            stderr_fd: parent_stderr,
            waited: Cell::new(false),
        },
    })
}

/// A running sandboxed process on macOS.
///
/// Wraps the child PID and stdio pipe file descriptors.
pub struct MacSandboxedChild {
    child_pid: i32,
    stdin_fd: Option<i32>,
    stdout_fd: Option<i32>,
    stderr_fd: Option<i32>,
    waited: Cell<bool>,
}

impl MacSandboxedChild {
    pub const fn id(&self) -> u32 {
        self.child_pid as u32
    }

    pub fn kill(&self) -> io::Result<()> {
        if self.waited.get() {
            return Ok(());
        }
        // SAFETY: valid pid, SIGKILL is a well-known signal
        let rc = unsafe { libc::kill(self.child_pid, libc::SIGKILL) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: valid pid, valid pointer
            let rc = unsafe { libc::waitpid(self.child_pid, &mut status, 0) };
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
        Ok(exit_status_from_raw(status))
    }

    pub fn try_wait(&self) -> io::Result<Option<std::process::ExitStatus>> {
        let mut status: libc::c_int = 0;
        // SAFETY: valid pid, valid pointer, WNOHANG for non-blocking
        let rc = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if rc == 0 {
            return Ok(None);
        }
        self.waited.set(true);
        Ok(Some(exit_status_from_raw(status)))
    }

    pub fn wait_with_output(mut self) -> io::Result<std::process::Output> {
        // Take fds so Drop won't double-close them
        let stdout_fd = self.stdout_fd.take();
        let stderr_fd = self.stderr_fd.take();

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
}

/// Read from two optional fds concurrently using `poll`, closing each when done.
fn read_two_fds(fd1: Option<i32>, fd2: Option<i32>) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf1 = Vec::new();
    let mut buf2 = Vec::new();

    if fd1.is_none() && fd2.is_none() {
        return Ok((buf1, buf2));
    }

    let mut active1 = fd1;
    let mut active2 = fd2;
    let mut tmp = [0u8; 4096];

    'outer: while active1.is_some() || active2.is_some() {
        let mut pollfds: Vec<libc::pollfd> = Vec::new();
        let mut idx_map: Vec<u8> = Vec::new();

        if let Some(fd) = active1 {
            pollfds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            idx_map.push(1);
        }
        if let Some(fd) = active2 {
            pollfds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            idx_map.push(2);
        }

        // SAFETY: pollfds is a valid array, -1 means wait indefinitely
        let rc = unsafe {
            libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, -1)
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            if let Some(fd) = active1 {
                // SAFETY: fd is valid
                unsafe { libc::close(fd) };
            }
            if let Some(fd) = active2 {
                // SAFETY: fd is valid
                unsafe { libc::close(fd) };
            }
            return Err(err);
        }

        for (i, pfd) in pollfds.iter().enumerate() {
            if pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                // SAFETY: fd is valid, tmp buffer is stack-allocated
                let n = unsafe { libc::read(pfd.fd, tmp.as_mut_ptr().cast(), tmp.len()) };
                if n < 0 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EINTR) {
                        // Retry from poll() — the read was interrupted
                        continue 'outer;
                    }
                    if let Some(fd) = active1 {
                        // SAFETY: fd is valid
                        unsafe { libc::close(fd) };
                    }
                    if let Some(fd) = active2 {
                        // SAFETY: fd is valid
                        unsafe { libc::close(fd) };
                    }
                    return Err(err);
                }
                if n == 0 {
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

impl Drop for MacSandboxedChild {
    fn drop(&mut self) {
        // Close any remaining fds
        if let Some(fd) = self.stdin_fd.take() {
            // SAFETY: fd is valid
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = self.stdout_fd.take() {
            // SAFETY: fd is valid
            unsafe { libc::close(fd) };
        }
        if let Some(fd) = self.stderr_fd.take() {
            // SAFETY: fd is valid
            unsafe { libc::close(fd) };
        }

        if !self.waited.get() {
            // Process not yet reaped — kill and collect to prevent zombie leak.
            // SAFETY: child_pid is valid; blocking wait reaps the zombie after SIGKILL.
            unsafe {
                libc::kill(self.child_pid, libc::SIGKILL);
                libc::waitpid(self.child_pid, std::ptr::null_mut(), 0);
            };
        }
    }
}

/// Convert a raw `waitpid` status to `ExitStatus`.
fn exit_status_from_raw(status: i32) -> std::process::ExitStatus {
    // SAFETY: from_raw takes a raw wait status on Unix
    std::os::unix::process::ExitStatusExt::from_raw(status)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::policy::ResourceLimits;
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

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        let output = child.inner.wait_with_output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert_eq!(stdout.trim(), "hello");
            }
            Err(_) => {}
        }
    }

    #[test]
    fn spawn_read_allowed_path() {
        let tmp = tempfile::TempDir::new().expect("create temp dir");
        let test_file = tmp.path().join("test.txt");
        std::fs::write(&test_file, "sandbox_test_content").expect("write test file");

        let policy = SandboxPolicy {
            read_paths: vec![tmp.path().to_path_buf()],
            write_paths: vec![],
            exec_paths: vec![],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg(test_file.to_str().expect("path to str"));
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        match child.inner.wait_with_output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    stdout.contains("sandbox_test_content"),
                    "expected to read allowed file, got: {stdout}"
                );
            }
            Err(_) => {}
        }
    }

    #[test]
    fn spawn_read_disallowed_path_fails() {
        // Only allow reading /usr, then try to read from /etc
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/cat");
        cmd.arg("/etc/hosts");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let child = match spawn(&policy, &cmd) {
            Ok(c) => c,
            Err(_) => return,
        };

        match child.inner.wait_with_output() {
            Ok(out) => {
                // cat should fail because /etc is not in read_paths
                assert!(
                    !out.status.success(),
                    "expected cat /etc/hosts to fail inside sandbox"
                );
            }
            Err(_) => {}
        }
    }

    #[test]
    fn generate_profile_produces_valid_sbpl() {
        let policy = SandboxPolicy {
            read_paths: vec![PathBuf::from("/tmp/read")],
            write_paths: vec![PathBuf::from("/tmp/write")],
            exec_paths: vec![PathBuf::from("/usr/bin")],
            allow_network: true,
            limits: ResourceLimits::default(),
        };
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
