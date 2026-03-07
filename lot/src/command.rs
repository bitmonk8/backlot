use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Stdio;

/// A command to run inside a sandbox.
pub struct SandboxCommand {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    /// Additional env vars. Platform essentials are always included.
    /// To forward a parent var, read it from `std::env` and pass it here.
    pub(crate) env: Vec<(OsString, OsString)>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) stdin: Stdio,
    pub(crate) stdout: Stdio,
    pub(crate) stderr: Stdio,
}

impl SandboxCommand {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: Stdio::null(),
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
        }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

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

    pub fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, val: V) -> &mut Self {
        self.env
            .push((key.as_ref().to_owned(), val.as_ref().to_owned()));
        self
    }

    pub fn cwd<P: Into<PathBuf>>(&mut self, dir: P) -> &mut Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn stdin(&mut self, stdin: Stdio) -> &mut Self {
        self.stdin = stdin;
        self
    }

    pub fn stdout(&mut self, stdout: Stdio) -> &mut Self {
        self.stdout = stdout;
        self
    }

    pub fn stderr(&mut self, stderr: Stdio) -> &mut Self {
        self.stderr = stderr;
        self
    }
}
