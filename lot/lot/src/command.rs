use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// How to handle a standard I/O stream for the sandboxed process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxStdio {
    /// Redirect to /dev/null (NUL on Windows).
    Null,
    /// Create an anonymous pipe between parent and child.
    Piped,
    /// Inherit the parent's handle.
    Inherit,
}

/// Describes the program, arguments, environment, working directory, and I/O
/// streams for a process that will be launched inside a sandbox.
///
/// Construct with [`SandboxCommand::new`], then configure via builder methods.
#[derive(Debug)]
pub struct SandboxCommand {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    /// Additional env vars. Platform essentials are always included.
    /// To forward a parent var, read it from `std::env` and pass it here.
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) stdin: SandboxStdio,
    pub(crate) stdout: SandboxStdio,
    pub(crate) stderr: SandboxStdio,
}

impl SandboxCommand {
    /// Create a new command targeting the given program executable.
    ///
    /// Stdin defaults to [`SandboxStdio::Null`], stdout and stderr to
    /// [`SandboxStdio::Piped`].
    ///
    /// The asymmetry is intentional: Null stdin prevents the child from
    /// hanging on a blocking read when no input is provided. Piped
    /// stdout/stderr is the most useful default because callers typically
    /// want to capture the child's output.
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: SandboxStdio::Null,
            stdout: SandboxStdio::Piped,
            stderr: SandboxStdio::Piped,
        }
    }

    /// Append a single argument to the command.
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Append multiple arguments to the command.
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self.args.push(arg.as_ref().to_owned());
        }
        self
    }

    /// Add an environment variable to the child process.
    pub fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, val: V) -> &mut Self {
        self.env
            .push((key.as_ref().to_owned(), val.as_ref().to_owned()));
        self
    }

    /// Set the working directory for the child process.
    pub fn cwd<P: Into<PathBuf>>(&mut self, dir: P) -> &mut Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Configure the child's standard input stream.
    pub const fn stdin(&mut self, stdio: SandboxStdio) -> &mut Self {
        self.stdin = stdio;
        self
    }

    /// Configure the child's standard output stream.
    pub const fn stdout(&mut self, stdio: SandboxStdio) -> &mut Self {
        self.stdout = stdio;
        self
    }

    /// Configure the child's standard error stream.
    pub const fn stderr(&mut self, stdio: SandboxStdio) -> &mut Self {
        self.stderr = stdio;
        self
    }

    /// Forward a standard set of environment variables from the parent
    /// process. Includes `PATH`, `HOME`, `USER`, `LANG`, `LC_ALL`, `TERM`,
    /// `SHELL`, `TMPDIR`, `TMP`, `TEMP`, `SYSTEMROOT`, `COMSPEC`, `WINDIR`,
    /// `PROGRAMFILES`, `APPDATA`, `LOCALAPPDATA`, `USERPROFILE`. Missing
    /// keys are silently skipped.
    ///
    /// The cross-platform key list avoids `#[cfg]` splitting — Unix keys
    /// are no-ops on Windows and vice versa.
    pub fn forward_common_env(&mut self) -> &mut Self {
        const KEYS: &[&str] = &[
            "PATH",
            "HOME",
            "USER",
            "LANG",
            "LC_ALL",
            "TERM",
            "SHELL",
            "TMPDIR",
            "TMP",
            "TEMP",
            "SYSTEMROOT",
            "COMSPEC",
            "WINDIR",
            "PROGRAMFILES",
            "APPDATA",
            "LOCALAPPDATA",
            "USERPROFILE",
        ];

        for key in KEYS {
            if let Ok(val) = std::env::var(key) {
                // Skip if the key was already set via .env() to avoid duplicates.
                // On Windows env var names are case-insensitive, so compare
                // with case folding there.
                let already_set = self.env.iter().any(|(k, _)| {
                    #[cfg(target_os = "windows")]
                    {
                        k.eq_ignore_ascii_case(OsStr::new(key))
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        k == key
                    }
                });
                if !already_set {
                    self.env(key, val);
                }
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Builder method tests ──────────────────────────────────────

    #[test]
    fn new_sets_defaults() {
        let cmd = SandboxCommand::new("test_program");
        assert_eq!(cmd.program, "test_program");
        assert!(cmd.args.is_empty());
        assert!(cmd.env.is_empty());
        assert!(cmd.cwd.is_none());
        assert_eq!(cmd.stdin, SandboxStdio::Null);
        assert_eq!(cmd.stdout, SandboxStdio::Piped);
        assert_eq!(cmd.stderr, SandboxStdio::Piped);
    }

    #[test]
    fn arg_appends_single() {
        let mut cmd = SandboxCommand::new("test");
        cmd.arg("one");
        assert_eq!(cmd.args.len(), 1);
        assert_eq!(cmd.args[0], "one");
    }

    #[test]
    fn args_appends_multiple() {
        let mut cmd = SandboxCommand::new("test");
        cmd.args(["a", "b", "c"]);
        assert_eq!(cmd.args.len(), 3);
        assert_eq!(cmd.args[0], "a");
        assert_eq!(cmd.args[2], "c");
    }

    #[test]
    fn env_adds_key_value() {
        let mut cmd = SandboxCommand::new("test");
        cmd.env("KEY", "VALUE");
        assert_eq!(cmd.env.len(), 1);
        assert_eq!(cmd.env[0].0, "KEY");
        assert_eq!(cmd.env[0].1, "VALUE");
    }

    #[test]
    fn cwd_sets_working_directory() {
        let mut cmd = SandboxCommand::new("test");
        cmd.cwd("/some/path");
        assert_eq!(cmd.cwd, Some(PathBuf::from("/some/path")));
    }

    #[test]
    fn stdin_sets_mode() {
        let mut cmd = SandboxCommand::new("test");
        cmd.stdin(SandboxStdio::Inherit);
        assert_eq!(cmd.stdin, SandboxStdio::Inherit);
    }

    #[test]
    fn stdout_sets_mode() {
        let mut cmd = SandboxCommand::new("test");
        cmd.stdout(SandboxStdio::Null);
        assert_eq!(cmd.stdout, SandboxStdio::Null);
    }

    #[test]
    fn stderr_sets_mode() {
        let mut cmd = SandboxCommand::new("test");
        cmd.stderr(SandboxStdio::Inherit);
        assert_eq!(cmd.stderr, SandboxStdio::Inherit);
    }

    // ── Hermetic forward_common_env tests ─────────────────────────

    #[test]
    fn forward_common_env_populates_from_parent() {
        let mut cmd = SandboxCommand::new("test");
        cmd.forward_common_env();

        // At least one of the common keys (e.g. PATH) should be present
        // in any CI or dev environment.
        assert!(
            !cmd.env.is_empty(),
            "expected at least one common env var to be forwarded"
        );

        // Every forwarded value must match the parent.
        for (key, val) in &cmd.env {
            if let Some(parent_val) = std::env::var_os(key) {
                assert_eq!(*val, parent_val);
            } else {
                panic!("forwarded key {key:?} not found in parent env");
            }
        }
    }

    #[test]
    fn forward_common_env_does_not_overwrite_existing() {
        let mut cmd = SandboxCommand::new("test");
        cmd.env("PATH", "custom_path_value");
        cmd.forward_common_env();

        // The manually-set PATH should be preserved, not overwritten.
        let path_entries: Vec<_> = cmd.env.iter().filter(|(k, _)| k == "PATH").collect();
        assert_eq!(path_entries.len(), 1, "PATH should appear exactly once");
        assert_eq!(path_entries[0].1, "custom_path_value");
    }

    #[test]
    fn forward_common_env_case_sensitivity() {
        // Verify that pre-set keys with different casing are handled correctly.
        let mut cmd = SandboxCommand::new("test");
        cmd.env("path", "lowercase_path");
        cmd.forward_common_env();

        let path_count = cmd
            .env
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(OsStr::new("PATH")))
            .count();

        #[cfg(target_os = "windows")]
        assert_eq!(
            path_count, 1,
            "Windows: case-insensitive dedup should prevent duplicate"
        );

        #[cfg(not(target_os = "windows"))]
        {
            // On Unix, "path" and "PATH" are different keys, so both should exist
            // (if PATH exists in the environment)
            if std::env::var("PATH").is_ok() {
                assert_eq!(path_count, 2, "Unix: case-sensitive, both should exist");
            }
        }
    }

    #[test]
    fn forward_common_env_is_additive() {
        let mut cmd = SandboxCommand::new("test");
        cmd.env("CUSTOM", "value");
        let before = cmd.env.len();
        cmd.forward_common_env();
        assert!(cmd.env.len() > before, "should add to existing env");
        assert_eq!(cmd.env[0].0, "CUSTOM");
    }
}
