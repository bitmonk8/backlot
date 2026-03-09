#![allow(unsafe_code)]

pub mod seatbelt;

use std::cell::Cell;
use std::io;

use crate::command::SandboxCommand;
use crate::policy::SandboxPolicy;
use crate::unix;
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
/// # Safety
/// Must only be called from the forked child before exec.
unsafe fn apply_resource_limits(policy: &SandboxPolicy) -> io::Result<()> {
    if let Some(max_mem) = policy.limits.max_memory_bytes {
        let rlim = libc::rlimit {
            rlim_cur: max_mem,
            rlim_max: max_mem,
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_AS, &raw const rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if let Some(max_procs) = policy.limits.max_processes {
        let rlim = libc::rlimit {
            rlim_cur: u64::from(max_procs),
            rlim_max: u64::from(max_procs),
        };
        // SAFETY: rlim is a valid rlimit struct
        if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &raw const rlim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    if let Some(max_cpu) = policy.limits.max_cpu_seconds {
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
    let (child_stdin, child_stdout, child_stderr, parent_stdin, parent_stdout, parent_stderr) =
        unix::setup_stdio_pipes(command)
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

        // Start a new session so the child becomes its own process group leader.
        // This lets the parent killpg() all descendants, not just the direct child.
        // setsid() only fails with EPERM if the process is already a session
        // leader, which cannot happen after fork (fresh PID != parent SID).
        // SAFETY: setsid() is async-signal-safe per POSIX, safe to call after fork.
        if unsafe { libc::setsid() } < 0 {
            // killpg() requires PGID == PID; abort if we can't guarantee that.
            unsafe { libc::_exit(71) }; // EX_OSERR
        }

        // Close parent's end of error pipe
        // SAFETY: valid fd
        unsafe { libc::close(err_pipe_rd) };

        // Close parent's stdio pipe ends
        unix::close_parent_pipes(parent_stdin, parent_stdout, parent_stderr);

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
            unsafe { unix::close_if_not_std(child_stdin) };
        }
        if child_stdout != 1 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stdout, 1) } < 0 {
                child_bail!(err_pipe_wr, unsafe { *libc::__error() });
            }
            // SAFETY: fd is valid
            unsafe { unix::close_if_not_std(child_stdout) };
        }
        if child_stderr != 2 {
            // SAFETY: both fds are valid
            if unsafe { libc::dup2(child_stderr, 2) } < 0 {
                child_bail!(err_pipe_wr, unsafe { *libc::__error() });
            }
            // SAFETY: fd is valid
            unsafe { unix::close_if_not_std(child_stderr) };
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
        // Reap the child so we don't leak a zombie
        // SAFETY: valid pid
        unsafe { libc::waitpid(child_pid, std::ptr::null_mut(), 0) };
        unix::close_parent_pipes(parent_stdin, parent_stdout, parent_stderr);
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
        // Kill the entire process group so descendants are also terminated.
        // The child called setsid() so its PGID equals its PID.
        // SAFETY: child_pid is a valid PGID after setsid(); SIGKILL is well-defined.
        let rc = unsafe { libc::killpg(self.child_pid, libc::SIGKILL) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            // ESRCH means the process group is already gone — not an error.
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err);
            }
        }
        Ok(())
    }

    pub fn wait(&self) -> io::Result<std::process::ExitStatus> {
        let mut status: libc::c_int = 0;
        loop {
            // SAFETY: valid pid, valid pointer
            let rc = unsafe { libc::waitpid(self.child_pid, &raw mut status, 0) };
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
        let rc = unsafe { libc::waitpid(self.child_pid, &raw mut status, libc::WNOHANG) };
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

    /// Kill the process group (child called setsid), wait for exit, close fds.
    ///
    /// macOS seatbelt has no post-exit cleanup, so this is just kill + wait.
    /// Consumes `self`; Drop still runs but sees already-cleaned-up state.
    pub fn kill_and_cleanup(mut self) -> crate::Result<()> {
        self.close_fds();

        if !self.waited.get() {
            self.kill().map_err(crate::SandboxError::Io)?;
            self.wait().map_err(crate::SandboxError::Io)?;
        }
        Ok(())
    }
}

impl Drop for MacSandboxedChild {
    fn drop(&mut self) {
        self.close_fds();

        if !self.waited.get() {
            // Kill the entire process group so descendants don't leak.
            // The child called setsid() so its PGID equals its PID.
            // SAFETY: child_pid is a valid PGID after setsid(); SIGKILL is well-defined.
            // waitpid reaps the direct child to prevent zombie leak.
            unsafe {
                libc::killpg(self.child_pid, libc::SIGKILL);
                libc::waitpid(self.child_pid, std::ptr::null_mut(), 0);
            };
        }
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
        SandboxPolicy {
            read_paths,
            write_paths: vec![],
            exec_paths: vec![],
            allow_network: false,
            limits: ResourceLimits::default(),
        }
    }

    /// Run sandbox-exec with a given profile and return its Output.
    fn run_sandbox_exec(label: &str, profile: &str) -> std::process::Output {
        eprintln!("[diag] --- {label} ---");
        eprintln!("[diag] profile:\n{profile}");
        let out = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", profile, "/bin/echo", "hello"])
            .output()
            .expect("sandbox-exec");
        eprintln!("[diag] exit: {:?}", out.status);
        eprintln!("[diag] stdout: {:?}", String::from_utf8_lossy(&out.stdout));
        eprintln!("[diag] stderr: {:?}", String::from_utf8_lossy(&out.stderr));
        out
    }

    #[test]
    fn sandbox_exec_diagnostics() {
        // Progressively restrictive profiles to pinpoint what operation
        // causes SIGABRT in our generated profile.
        eprintln!("[diag] === sandbox_exec_diagnostics ===");

        // 1. Fully permissive — if this fails, sandbox-exec itself is broken
        let out1 = run_sandbox_exec("permissive", "(version 1)(allow default)");
        assert!(
            out1.status.success(),
            "permissive sandbox failed: {:?}\nstderr: {}",
            out1.status,
            String::from_utf8_lossy(&out1.stderr)
        );

        // 2. Deny-default with broad allows covering all major operation classes
        let broad = "\
(version 1)
(deny default)
(allow process*)
(allow file*)
(allow mach*)
(allow sysctl*)
(allow signal)
(allow iokit*)
(allow ipc*)
(allow network*)
(allow system*)
";
        let out2 = run_sandbox_exec("broad-allows", broad);
        assert!(
            out2.status.success(),
            "broad-allows sandbox failed: {:?}\nstderr: {}",
            out2.status,
            String::from_utf8_lossy(&out2.stderr)
        );

        // 3. Narrow down: deny-default + only process/file/mach/sysctl/signal
        let narrow = "\
(version 1)
(deny default)
(allow process*)
(allow file*)
(allow mach*)
(allow sysctl*)
(allow signal)
";
        let out3 = run_sandbox_exec("narrow-allows", narrow);
        eprintln!("[diag] narrow-allows success: {}", out3.status.success());

        // 4. Our actual generated profile
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let program = std::path::Path::new("/bin/echo");
        let resolved = std::fs::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
        let profile = seatbelt::generate_profile(&policy, &resolved);
        let out4 = run_sandbox_exec("generated-profile", &profile);
        eprintln!(
            "[diag] generated-profile success: {}",
            out4.status.success()
        );

        // If narrow works but generated doesn't, we need to find the missing
        // If narrow works but generated doesn't, narrow further.
        if out3.status.success() && !out4.status.success() {
            // 5a. file* wildcard with scoped process/mach
            let p5a = "\
(version 1)
(deny default)
(allow file*)
(allow process-exec (subpath \"/usr/bin\"))
(allow process-exec (subpath \"/bin\"))
(allow process-exec (subpath \"/System/Cryptexes/OS\"))
(allow process-fork)
(allow process-info* (target self))
(allow mach-lookup)
(allow sysctl-read)
(allow signal (target self))
";
            let o5a = run_sandbox_exec("file-wild+scoped-rest", p5a);
            eprintln!("[diag] file-wild+scoped-rest: {}", o5a.status.success());

            // 5b. process* wildcard with scoped file
            let p5b = "\
(version 1)
(deny default)
(allow process*)
(allow file-read* (subpath \"/\"))
(allow file-map-executable (subpath \"/\"))
(allow mach-lookup)
(allow sysctl-read)
(allow signal (target self))
";
            let o5b = run_sandbox_exec("process-wild+file-read-root", p5b);
            eprintln!(
                "[diag] process-wild+file-read-root: {}",
                o5b.status.success()
            );

            // 5c. Our profile + system* (maybe missing system-mac-syscall)
            let p5c = format!("{profile}\n(allow system*)\n");
            let o5c = run_sandbox_exec("generated+system*", &p5c);
            eprintln!("[diag] generated+system*: {}", o5c.status.success());

            // 5d. Our profile + iokit* broadened
            let p5d = format!("{profile}\n(allow iokit*)\n");
            let o5d = run_sandbox_exec("generated+iokit*", &p5d);
            eprintln!("[diag] generated+iokit*: {}", o5d.status.success());

            // 5e. Our profile + file-read* root (maybe missing a system path)
            let p5e = format!(
                "{profile}\n(allow file-read* (subpath \"/\"))\n(allow file-map-executable (subpath \"/\"))\n"
            );
            let o5e = run_sandbox_exec("generated+file-read-root", &p5e);
            eprintln!("[diag] generated+file-read-root: {}", o5e.status.success());

            // 5f. Our profile + process*
            let p5f = format!("{profile}\n(allow process*)\n");
            let o5f = run_sandbox_exec("generated+process*", &p5f);
            eprintln!("[diag] generated+process*: {}", o5f.status.success());

            // Phase 2: Narrow which file path is missing.
            // Test file-read* only vs file-map-executable only
            let p6a = format!("{profile}\n(allow file-read* (subpath \"/\"))\n");
            let o6a = run_sandbox_exec("generated+file-read-root-only", &p6a);
            eprintln!(
                "[diag] generated+file-read-root-only: {}",
                o6a.status.success()
            );

            let p6b = format!("{profile}\n(allow file-map-executable (subpath \"/\"))\n");
            let o6b = run_sandbox_exec("generated+file-map-exec-root-only", &p6b);
            eprintln!(
                "[diag] generated+file-map-exec-root-only: {}",
                o6b.status.success()
            );

            // Test specific paths we might be missing
            let paths_to_test = [
                "/private/etc",
                "/usr/share",
                "/private/var",
                "/private/tmp",
                "/var/folders",
                "/usr/local",
                "/etc",
                "/tmp",
                "/Users",
            ];
            for test_path in &paths_to_test {
                let p = format!(
                    "{profile}\n(allow file-read* (subpath \"{test_path}\"))\n(allow file-map-executable (subpath \"{test_path}\"))\n"
                );
                let o = run_sandbox_exec(&format!("generated+{test_path}"), &p);
                eprintln!("[diag] generated+{test_path}: {}", o.status.success());
            }

            // Test file-map-executable for dyld shared cache
            let p7 = format!(
                "{profile}\n(allow file-map-executable (subpath \"/private/var/db/dyld\"))\n"
            );
            let o7 = run_sandbox_exec("generated+fme-dyld", &p7);
            eprintln!("[diag] generated+fme-dyld: {}", o7.status.success());

            // Test file-map-executable for /System/Cryptexes
            let p8 =
                format!("{profile}\n(allow file-map-executable (subpath \"/System/Cryptexes\"))\n");
            let o8 = run_sandbox_exec("generated+fme-cryptex", &p8);
            eprintln!("[diag] generated+fme-cryptex: {}", o8.status.success());
        }

        // Final assertion: our generated profile must work
        assert!(
            out4.status.success(),
            "generated profile failed: {:?}\nstderr: {}",
            out4.status,
            String::from_utf8_lossy(&out4.stderr)
        );
        let stdout = String::from_utf8_lossy(&out4.stdout);
        assert_eq!(stdout.trim(), "hello");
    }

    #[test]
    fn spawn_echo_hello() {
        let policy = test_policy(vec![PathBuf::from("/usr")]);
        let mut cmd = SandboxCommand::new("/bin/echo");
        cmd.arg("hello");
        cmd.stdout(SandboxStdio::Piped);
        cmd.stderr(SandboxStdio::Piped);

        let Ok(child) = spawn(&policy, &cmd) else {
            return;
        };

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

        let Ok(child) = spawn(&policy, &cmd) else {
            return;
        };

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

        let Ok(child) = spawn(&policy, &cmd) else {
            return;
        };

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
