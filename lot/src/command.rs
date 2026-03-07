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
}
