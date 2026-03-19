//! Cross-platform process sandboxing library.
//!
//! Provides OS-level isolation for spawned child processes. The caller is never
//! sandboxed; only the child inherits the restrictions.
//!
//! # Supported platforms
//!
//! - **Linux** — user/mount/PID/network namespaces, seccomp-BPF, cgroups v2
//! - **macOS** — Seatbelt (`sandbox_init`) profiles
//! - **Windows** — `AppContainer` profiles, Job Objects
//!
//! # Key functions
//!
//! - [`spawn()`] — launch a child process inside a sandbox defined by a [`SandboxPolicy`].
//! - [`probe()`] — detect which sandboxing mechanisms are available on the current platform.
//! - [`cleanup_stale()`] — restore ACLs from crashed sessions (Windows; no-op elsewhere).

mod command;
mod error;
mod policy;
mod policy_builder;

#[cfg(unix)]
mod unix;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub use command::{SandboxCommand, SandboxStdio};
pub use error::SandboxError;
pub use policy::{ResourceLimits, SandboxPolicy};
pub use policy_builder::SandboxPolicyBuilder;

/// Result type for sandbox operations.
pub type Result<T> = std::result::Result<T, SandboxError>;

/// Reports which OS-level sandboxing mechanisms are available on the current
/// platform. Obtained via [`probe()`].
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct PlatformCapabilities {
    /// Linux: user namespaces available and permitted.
    pub namespaces: bool,
    /// Linux: seccomp-BPF available.
    pub seccomp: bool,
    /// Linux: cgroups v2 delegation available for the current user.
    pub cgroups_v2: bool,
    /// macOS: Seatbelt (`sandbox_init`) available.
    pub seatbelt: bool,
    /// Windows: `AppContainer` available.
    pub appcontainer: bool,
    /// Windows: Job objects available.
    pub job_objects: bool,
}

/// Check what sandboxing mechanisms are available on the current platform.
#[allow(clippy::missing_const_for_fn)]
pub fn probe() -> PlatformCapabilities {
    #[cfg(target_os = "linux")]
    return linux::probe();

    #[cfg(target_os = "macos")]
    return macos::probe();

    #[cfg(target_os = "windows")]
    return windows::probe();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: false,
        appcontainer: false,
        job_objects: false,
    };
}

/// Spawn a sandboxed child process.
///
/// The caller is never sandboxed. The child process inherits
/// the sandbox restrictions and cannot escape them.
pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    policy.validate()?;
    check_env_coverage(policy, command)?;

    #[cfg(target_os = "windows")]
    return windows::spawn(policy, command);

    #[cfg(target_os = "linux")]
    return linux::spawn(policy, command);

    #[cfg(target_os = "macos")]
    return macos::spawn(policy, command);

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = command;
        Err(SandboxError::Unsupported("not yet implemented".into()))
    }
}

/// Check that the child's effective TEMP/TMP/TMPDIR and PATH env vars
/// reference directories the sandbox can actually access. Returns
/// `InvalidPolicy` with actionable guidance if any are unreachable.
///
/// A directory is considered accessible if it falls under a policy grant path
/// OR under a platform-implicit path (system dirs each platform auto-mounts
/// or allows by default).
fn check_env_coverage(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<()> {
    /// Check if `dir` is covered by a policy grant path or a platform-implicit path.
    fn is_accessible(
        dir: &std::path::Path,
        grant: &[&std::path::Path],
        implicit: &[std::path::PathBuf],
    ) -> bool {
        grant.iter().any(|g| path_is_under(g, dir))
            || implicit.iter().any(|g| path_is_under(g, dir))
    }

    let mut errors: Vec<String> = Vec::new();

    let implicit = platform_implicit_read_paths();
    let grant = policy.grant_paths();

    // Resolve the effective value of an env var as the child will see it.
    let effective_env = |key: &str| -> Option<std::ffi::OsString> {
        // Explicit override in command.env takes priority.
        for (k, v) in &command.env {
            let matches = {
                #[cfg(target_os = "windows")]
                {
                    k.eq_ignore_ascii_case(std::ffi::OsStr::new(key))
                }
                #[cfg(not(target_os = "windows"))]
                {
                    *k == *key
                }
            };
            if matches {
                return Some(v.clone());
            }
        }
        // Inherited env: Windows inherits parent env when command.env is empty.
        // Unix builds an explicit envp — no inheritance, but a default PATH.
        // Intentional: on Windows with empty env, this reads the parent's
        // TEMP/TMP (typically C:\Users\...\AppData\Local\Temp) and requires
        // it in write_paths. Callers must either add system temp as a
        // write_path or override TEMP/TMP via SandboxCommand::env().
        #[cfg(target_os = "windows")]
        if command.env.is_empty() {
            return std::env::var_os(key);
        }
        #[cfg(not(target_os = "windows"))]
        if command.env.is_empty() && key == "PATH" {
            return Some(std::ffi::OsString::from(
                "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            ));
        }
        None
    };

    // TEMP/TMP/TMPDIR must be under a write path (temp dirs need write access).
    // Platform-implicit paths are read-only, so they don't satisfy temp.
    for key in &["TEMP", "TMP", "TMPDIR"] {
        if let Some(val) = effective_env(key) {
            let dir = std::path::Path::new(&val);
            if !dir.as_os_str().is_empty() && !policy_covers_path(policy.write_paths(), dir) {
                errors.push(format!(
                    "{key}={} is not covered by any write_path in the policy. \
                     Either add it as a write_path or override it with \
                     SandboxCommand::env(\"{key}\", <a granted path>)",
                    dir.display()
                ));
            }
        }
    }

    // PATH entries must be readable (covered by a grant path or platform-implicit).
    if let Some(val) = effective_env("PATH") {
        let uncovered: Vec<String> = std::env::split_paths(&val)
            .filter(|entry| !entry.as_os_str().is_empty())
            .filter(|entry| !is_accessible(entry, &grant, &implicit))
            .map(|entry| entry.display().to_string())
            .collect();
        if !uncovered.is_empty() {
            errors.push(format!(
                "{} PATH entries are not accessible to the sandbox (first: {}). \
                 Either add them as read_path/exec_path or override PATH with \
                 SandboxCommand::env(\"PATH\", <accessible paths only>)",
                uncovered.len(),
                uncovered[0]
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SandboxError::InvalidPolicy(errors.join("; ")))
    }
}

/// Directories each platform makes accessible to sandboxed processes
/// regardless of what the policy grants. These are auto-mounted (Linux),
/// allowed by default in the SBPL profile (macOS), or readable by all
/// AppContainer processes (Windows).
fn platform_implicit_read_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "linux")]
    {
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
    }

    #[cfg(target_os = "macos")]
    {
        for p in &[
            "/usr/lib",
            "/usr/bin",
            "/bin",
            "/sbin",
            "/usr/sbin",
            "/usr/local/bin",
            "/System/Library",
            "/System/Cryptexes",
        ] {
            let path = std::path::Path::new(p);
            if path.exists() {
                paths.push(path.to_path_buf());
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // AppContainer processes can read system directories by default.
        let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
        // sys_root covers all System32 subdirs via path_is_under ancestor check.
        paths.push(std::path::PathBuf::from(&sys_root));
    }

    paths
}

/// Check if `dir` is equal to or a descendant of any path in `paths`.
fn policy_covers_path(paths: &[std::path::PathBuf], dir: &std::path::Path) -> bool {
    paths.iter().any(|grant| path_is_under(grant, dir))
}

/// True if `child` is equal to `parent` or a descendant of it.
fn path_is_under(parent: &std::path::Path, child: &std::path::Path) -> bool {
    // Try canonicalize first to resolve symlinks.
    let canon_parent = std::fs::canonicalize(parent);
    let canon_child = std::fs::canonicalize(child);
    if let (Ok(cp), Ok(cc)) = (&canon_parent, &canon_child) {
        return cc.starts_with(cp);
    }
    // Fall back to lexical comparison when canonicalize fails (path may not exist yet).
    let np = normalize_lexical(parent);
    let nc = normalize_lexical(child);
    nc.starts_with(&np)
}

/// Normalize a path lexically: resolve `.` and `..` components, normalize separators.
/// Does NOT touch the filesystem.
fn normalize_lexical(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {} // skip `.`
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Restore ACLs from any stale sentinel files left by crashed sessions (Windows).
/// No-op on other platforms.
#[allow(clippy::missing_const_for_fn)] // Not const on Windows.
pub fn cleanup_stale() -> Result<()> {
    #[cfg(target_os = "windows")]
    return windows::cleanup_stale();

    #[cfg(not(target_os = "windows"))]
    Ok(())
}

#[cfg(target_os = "windows")]
pub use windows::elevation::is_elevated;
#[cfg(target_os = "windows")]
pub use windows::nul_device::{
    appcontainer_prerequisites_met, appcontainer_prerequisites_met_for_policy,
    grant_appcontainer_prerequisites, grant_appcontainer_prerequisites_for_policy,
};

/// Grants AppContainer prerequisites for all paths in the policy.
/// No-op on non-Windows platforms.
#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)]
pub fn grant_appcontainer_prerequisites_for_policy(_policy: &SandboxPolicy) -> Result<()> {
    Ok(())
}

/// Checks whether prerequisites are met for all paths in the policy.
/// Always returns `true` on non-Windows platforms.
#[cfg(not(target_os = "windows"))]
#[allow(clippy::missing_const_for_fn)]
pub fn appcontainer_prerequisites_met_for_policy(_policy: &SandboxPolicy) -> bool {
    true
}

/// A running sandboxed process.
///
/// Created by [`spawn()`]. Dropping the handle performs platform-specific
/// cleanup (ACL restoration on Windows, cgroup removal on Linux, process
/// group termination on macOS).
pub struct SandboxedChild {
    #[cfg(target_os = "windows")]
    inner: windows::WindowsSandboxedChild,
    #[cfg(target_os = "linux")]
    inner: linux::LinuxSandboxedChild,
    #[cfg(target_os = "macos")]
    inner: macos::MacSandboxedChild,
}

impl std::fmt::Debug for SandboxedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedChild")
            .field("pid", &self.id())
            .finish()
    }
}

/// Macro to dispatch a method call to `self.inner` on supported platforms,
/// returning a fallback on unsupported ones.
macro_rules! platform_dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* ), $fallback:expr) => {{
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        return $self.inner.$method($($arg),*);

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        $fallback
    }};
}

impl SandboxedChild {
    /// Returns the OS-assigned process ID.
    pub const fn id(&self) -> u32 {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        return self.inner.id();

        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        0
    }

    /// Forcibly terminate the sandboxed process.
    pub fn kill(&mut self) -> std::io::Result<()> {
        platform_dispatch!(
            self,
            kill(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Block until the sandboxed process exits and return its exit status.
    pub fn wait(&self) -> std::io::Result<std::process::ExitStatus> {
        platform_dispatch!(
            self,
            wait(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Non-blocking check: has the process exited?
    pub fn try_wait(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        platform_dispatch!(
            self,
            try_wait(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Wait for the process to exit and collect all stdout/stderr output.
    pub fn wait_with_output(self) -> std::io::Result<std::process::Output> {
        platform_dispatch!(
            self,
            wait_with_output(),
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "not implemented"
            ))
        )
    }

    /// Take ownership of the child's stdin pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        platform_dispatch!(self, take_stdin(), None)
    }

    /// Take ownership of the child's stdout pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        platform_dispatch!(self, take_stdout(), None)
    }

    /// Take ownership of the child's stderr pipe (if piped).
    #[allow(clippy::missing_const_for_fn)] // not const on all platforms
    pub fn take_stderr(&mut self) -> Option<std::fs::File> {
        platform_dispatch!(self, take_stderr(), None)
    }

    /// Kill the sandboxed process (and all descendants), then run
    /// platform cleanup synchronously. Returns after cleanup is complete.
    ///
    /// Consumes `self` so the `Drop` impl does not run again.
    pub fn kill_and_cleanup(self) -> Result<()> {
        platform_dispatch!(
            self,
            kill_and_cleanup(),
            Err(SandboxError::Unsupported("not yet implemented".into()))
        )
    }

    /// Wait for the child to exit with a timeout. On timeout, kills the
    /// child (and all descendants), runs platform cleanup, and returns
    /// a timeout error.
    ///
    /// Internally uses `spawn_blocking` so the synchronous `wait_with_output`
    /// does not block the tokio runtime. On timeout the child is killed by
    /// raw PID to unblock the waiting thread, then the `JoinHandle` is
    /// awaited to ensure `Drop` cleanup runs before returning.
    #[cfg(feature = "tokio")]
    pub async fn wait_with_output_timeout(
        self,
        timeout: std::time::Duration,
    ) -> Result<std::process::Output> {
        let pid = self.id();

        let handle = tokio::task::spawn_blocking(move || self.wait_with_output());

        // Pin the handle so we can poll it across select! arms without
        // consuming it on the timeout path.
        tokio::pin!(handle);

        tokio::select! {
            join_result = &mut handle => {
                let io_result = join_result.unwrap_or_else(|join_err| {
                    Err(std::io::Error::other(join_err.to_string()))
                });
                Ok(io_result?)
            }
            () = tokio::time::sleep(timeout) => {
                // Timeout fired — kill the child by raw PID to unblock
                // the blocking thread's wait call.
                kill_by_pid(pid);

                // Await the blocking thread so Drop (platform cleanup)
                // runs to completion before we return.
                match handle.await {
                    Err(join_err) if join_err.is_panic() => {
                        std::panic::resume_unwind(join_err.into_panic());
                    }
                    _ => {}
                }

                Err(SandboxError::Timeout(timeout))
            }
        }
    }
}

/// Send a kill signal to a process by raw PID. Best-effort; errors are
/// ignored because the process may have already exited.
#[cfg(feature = "tokio")]
#[allow(unsafe_code)]
fn kill_by_pid(pid: u32) {
    #[cfg(unix)]
    {
        let Some(pid_i32) = i32::try_from(pid).ok().filter(|&p| p > 0) else {
            return;
        };
        // macOS children call setsid(), so PGID == PID — negate to kill
        // the entire process group. Linux uses PID namespaces instead;
        // killing the helper collapses the namespace.
        #[cfg(target_os = "macos")]
        let target = -pid_i32;
        #[cfg(not(target_os = "macos"))]
        let target = pid_i32;
        // SAFETY: Sending SIGKILL to a valid pid (or negated PGID on macOS).
        unsafe {
            libc::kill(target, libc::SIGKILL);
        }
    }

    #[cfg(windows)]
    {
        // SAFETY: Opening a process handle by PID and terminating it.
        // The handle is closed immediately after. Best-effort; the
        // process may have already exited. On Windows the Job Object
        // (KILL_ON_JOB_CLOSE) handles descendant cleanup when the
        // SandboxedChild is dropped on the blocking thread.
        unsafe {
            let h = windows_sys::Win32::System::Threading::OpenProcess(
                windows_sys::Win32::System::Threading::PROCESS_TERMINATE,
                0,
                pid,
            );
            if !h.is_null() {
                windows_sys::Win32::System::Threading::TerminateProcess(h, 1);
                windows_sys::Win32::Foundation::CloseHandle(h);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "windows")]
    fn probe_windows() {
        let caps = probe();
        assert!(caps.appcontainer);
        assert!(caps.job_objects);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn probe_linux_no_panic() {
        // Detection must not panic regardless of environment.
        let _caps = probe();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn probe_macos() {
        let caps = probe();
        assert!(caps.seatbelt);
    }

    #[test]
    fn check_env_coverage_ok_when_temp_in_write_path() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");
        let read_dir = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![read_dir.path().to_path_buf()],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![],
            false,
            crate::policy::ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", write_dir.path());
        // Set PATH to a platform-implicit directory so it passes.
        #[cfg(target_os = "windows")]
        {
            let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", format!(r"{sys_root}\System32"));
        }
        #[cfg(not(target_os = "windows"))]
        cmd.env("PATH", "/usr/bin");

        assert!(
            check_env_coverage(&policy, &cmd).is_ok(),
            "TEMP in write_path should pass"
        );
    }

    #[test]
    fn check_env_coverage_rejects_temp_outside_write_paths() {
        let read_dir = tempfile::TempDir::new().expect("create temp dir");
        let uncovered = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![read_dir.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            false,
            crate::policy::ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", uncovered.path());
        #[cfg(target_os = "windows")]
        {
            let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", format!(r"{sys_root}\System32"));
        }
        #[cfg(not(target_os = "windows"))]
        cmd.env("PATH", "/usr/bin");

        let err = check_env_coverage(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TEMP"), "error should mention TEMP: {msg}");
    }

    #[test]
    fn check_env_coverage_rejects_uncovered_path_entry() {
        let write_dir = tempfile::TempDir::new().expect("create temp dir");
        let uncovered = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![],
            vec![write_dir.path().to_path_buf()],
            vec![],
            vec![],
            false,
            crate::policy::ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", write_dir.path());
        cmd.env("PATH", uncovered.path());

        let err = check_env_coverage(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("PATH"), "error should mention PATH: {msg}");
    }

    #[test]
    fn check_env_coverage_accumulates_multiple_errors() {
        let read_dir = tempfile::TempDir::new().expect("create temp dir");
        let bad_temp = tempfile::TempDir::new().expect("create temp dir");
        let bad_path = tempfile::TempDir::new().expect("create temp dir");

        let policy = SandboxPolicy::new(
            vec![read_dir.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            false,
            crate::policy::ResourceLimits::default(),
        );

        let mut cmd = SandboxCommand::new("dummy");
        cmd.env("TEMP", bad_temp.path());
        cmd.env("PATH", bad_path.path());

        let err = check_env_coverage(&policy, &cmd).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TEMP"), "error should mention TEMP: {msg}");
        assert!(msg.contains("PATH"), "error should mention PATH: {msg}");
    }

    #[test]
    fn path_is_under_equal_paths() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        assert!(path_is_under(dir.path(), dir.path()));
    }

    #[test]
    fn path_is_under_child_is_descendant() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let child = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&child).expect("create subdirs");
        assert!(path_is_under(dir.path(), &child));
    }

    #[test]
    fn path_is_under_child_is_not_under_parent() {
        let a = tempfile::TempDir::new().expect("create temp dir");
        let b = tempfile::TempDir::new().expect("create temp dir");
        assert!(!path_is_under(a.path(), b.path()));
    }

    #[test]
    fn path_is_under_nonexistent_path_uses_lexical_fallback() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        // This child does not exist on disk, so canonicalize will fail.
        // The lexical fallback should still detect it as under `dir`.
        let nonexistent = dir.path().join("does_not_exist").join("nested");
        assert!(path_is_under(dir.path(), &nonexistent));
    }

    #[test]
    fn timeout_error_display() {
        let err = SandboxError::Timeout(std::time::Duration::from_secs(5));
        let msg = err.to_string();
        assert!(
            msg.contains("5s"),
            "expected duration in message, got: {msg}"
        );
    }

    #[test]
    fn normalize_lexical_resolves_dot() {
        #[cfg(target_os = "windows")]
        let input = std::path::Path::new(r"C:\a\.\b");
        #[cfg(not(target_os = "windows"))]
        let input = std::path::Path::new("/a/./b");

        let result = normalize_lexical(input);

        #[cfg(target_os = "windows")]
        assert_eq!(result, std::path::PathBuf::from(r"C:\a\b"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, std::path::PathBuf::from("/a/b"));
    }

    #[test]
    fn normalize_lexical_resolves_parent() {
        #[cfg(target_os = "windows")]
        let input = std::path::Path::new(r"C:\a\b\..\c");
        #[cfg(not(target_os = "windows"))]
        let input = std::path::Path::new("/a/b/../c");

        let result = normalize_lexical(input);

        #[cfg(target_os = "windows")]
        assert_eq!(result, std::path::PathBuf::from(r"C:\a\c"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, std::path::PathBuf::from("/a/c"));
    }

    #[test]
    fn normalize_lexical_plain_absolute_path() {
        #[cfg(target_os = "windows")]
        let input = std::path::Path::new(r"C:\a\b\c");
        #[cfg(not(target_os = "windows"))]
        let input = std::path::Path::new("/a/b/c");

        let result = normalize_lexical(input);

        #[cfg(target_os = "windows")]
        assert_eq!(result, std::path::PathBuf::from(r"C:\a\b\c"));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(result, std::path::PathBuf::from("/a/b/c"));
    }
}

#[cfg(test)]
#[cfg(feature = "tokio")]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tokio_tests {
    use super::*;
    use tempfile::TempDir;

    /// Create temp dir inside the project to avoid system temp ancestors
    /// (e.g. `C:\Users`) that require elevation for traverse ACE grants.
    fn make_temp_dir() -> TempDir {
        let test_tmp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp dir");
        TempDir::new_in(&test_tmp).expect("create temp dir")
    }

    /// Set sandbox-safe env overrides on Windows, no-op on Unix.
    #[cfg(target_os = "windows")]
    fn set_sandbox_env(cmd: &mut SandboxCommand, scratch: &std::path::Path) {
        let sys_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".into());
        let sys32 = format!(r"{sys_root}\System32");
        cmd.env("PATH", &sys32);
        cmd.env("TEMP", scratch);
        cmd.env("TMP", scratch);
        cmd.env("TMPDIR", scratch);
        cmd.forward_common_env();
    }

    #[cfg(not(target_os = "windows"))]
    fn set_sandbox_env(_cmd: &mut SandboxCommand, _scratch: &std::path::Path) {}

    /// Helper: build a minimal policy and command for timeout tests.
    /// Returns the child plus temp dir handles that must outlive the child.
    fn spawn_sleep(seconds: u32) -> (SandboxedChild, Vec<TempDir>) {
        #[cfg(unix)]
        {
            let policy = SandboxPolicy::new(
                vec![std::path::PathBuf::from("/usr")],
                vec![],
                vec![],
                vec![],
                false,
                crate::policy::ResourceLimits::default(),
            );
            let mut cmd = SandboxCommand::new("/bin/sleep");
            cmd.arg(seconds.to_string());
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);
            (
                spawn(&policy, &cmd).expect("spawn_sleep must succeed"),
                vec![],
            )
        }

        #[cfg(windows)]
        {
            // On Windows, use `powershell Start-Sleep` as a sleep substitute.
            let tmp = make_temp_dir();
            let scratch = make_temp_dir();
            let policy = SandboxPolicy::new(
                vec![tmp.path().to_path_buf()],
                vec![scratch.path().to_path_buf()],
                vec![],
                vec![],
                false,
                crate::policy::ResourceLimits::default(),
            );
            let mut cmd = SandboxCommand::new("powershell");
            cmd.args(["-Command", &format!("Start-Sleep -Seconds {seconds}")]);
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);
            set_sandbox_env(&mut cmd, scratch.path());
            (
                spawn(&policy, &cmd).expect("spawn_sleep must succeed"),
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
            Err(SandboxError::Timeout(d)) => {
                assert!(
                    d.as_millis() >= 200,
                    "timeout duration should match requested"
                );
            }
            other => panic!("expected Timeout error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fast_child_completes_before_timeout() {
        #[cfg(unix)]
        {
            let policy = SandboxPolicy::new(
                vec![std::path::PathBuf::from("/usr")],
                vec![],
                vec![],
                vec![],
                false,
                crate::policy::ResourceLimits::default(),
            );
            let mut cmd = SandboxCommand::new("/bin/echo");
            cmd.arg("hello");
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);

            let child = spawn(&policy, &cmd).expect("spawn must succeed");

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
            let policy = SandboxPolicy::new(
                vec![tmp.path().to_path_buf()],
                vec![scratch.path().to_path_buf()],
                vec![],
                vec![],
                false,
                crate::policy::ResourceLimits::default(),
            );
            let mut cmd = SandboxCommand::new("cmd.exe");
            cmd.args(["/C", "echo hello"]);
            cmd.stdout(SandboxStdio::Piped);
            cmd.stderr(SandboxStdio::Piped);
            set_sandbox_env(&mut cmd, scratch.path());

            let child = spawn(&policy, &cmd).expect("spawn must succeed");

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
