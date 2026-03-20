#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
use windows_sys::Win32::System::Pipes::CreatePipe;

use crate::command::SandboxStdio;

// ── Pipe helpers ─────────────────────────────────────────────────────

pub struct PipeHandles {
    pub read: HANDLE,
    pub write: HANDLE,
}

pub fn create_pipe() -> io::Result<PipeHandles> {
    #[allow(clippy::cast_possible_truncation)]
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };

    let mut read_handle: HANDLE = INVALID_HANDLE_VALUE;
    let mut write_handle: HANDLE = INVALID_HANDLE_VALUE;

    // SAFETY: Creating an anonymous pipe with inheritable handles.
    // Both handles are inheritable, but PROC_THREAD_ATTRIBUTE_HANDLE_LIST
    // restricts which handles the child actually inherits.
    let ret = unsafe { CreatePipe(&raw mut read_handle, &raw mut write_handle, &raw mut sa, 0) };
    if ret == FALSE {
        return Err(io::Error::last_os_error());
    }

    Ok(PipeHandles {
        read: read_handle,
        write: write_handle,
    })
}

pub fn resolve_stdio_input(spec: SandboxStdio) -> io::Result<(HANDLE, Option<HANDLE>)> {
    match spec {
        SandboxStdio::Null => Ok((INVALID_HANDLE_VALUE, None)),
        SandboxStdio::Inherit => {
            // SAFETY: Querying the current process's stdin handle.
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            Ok((handle, None))
        }
        SandboxStdio::Piped => {
            let pipe = create_pipe()?;
            // Child reads from read end, parent writes to write end.
            Ok((pipe.read, Some(pipe.write)))
        }
    }
}

pub fn resolve_stdio_output(
    spec: SandboxStdio,
    std_handle_id: u32,
) -> io::Result<(HANDLE, Option<HANDLE>)> {
    match spec {
        SandboxStdio::Null => Ok((INVALID_HANDLE_VALUE, None)),
        SandboxStdio::Inherit => {
            // SAFETY: Querying the current process's stdout/stderr handle.
            let handle = unsafe { GetStdHandle(std_handle_id) };
            Ok((handle, None))
        }
        SandboxStdio::Piped => {
            let pipe = create_pipe()?;
            // Child writes to write end, parent reads from read end.
            Ok((pipe.write, Some(pipe.read)))
        }
    }
}

pub fn close_handle_if_valid(h: HANDLE) {
    if h != INVALID_HANDLE_VALUE && !h.is_null() {
        // SAFETY: Closing a valid handle.
        unsafe {
            CloseHandle(h);
        }
    }
}

pub fn close_optional_handle(h: Option<HANDLE>) {
    if let Some(handle) = h {
        close_handle_if_valid(handle);
    }
}

/// Bundle of child and parent pipe handles for stdin/stdout/stderr.
pub struct StdioPipes {
    pub child_stdin: HANDLE,
    pub parent_stdin: Option<HANDLE>,
    pub child_stdout: HANDLE,
    pub parent_stdout: Option<HANDLE>,
    pub child_stderr: HANDLE,
    pub parent_stderr: Option<HANDLE>,
}

impl StdioPipes {
    /// Close all pipe handles. Used on error paths before the handles have
    /// been transferred to `File` ownership.
    pub fn close_all(&self) {
        close_optional_handle(self.parent_stdin);
        close_handle_if_valid(self.child_stdin);
        close_optional_handle(self.parent_stdout);
        close_handle_if_valid(self.child_stdout);
        close_optional_handle(self.parent_stderr);
        close_handle_if_valid(self.child_stderr);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn create_pipe_returns_valid_handles() {
        let pipe = create_pipe().unwrap();
        assert_ne!(pipe.read, INVALID_HANDLE_VALUE);
        assert_ne!(pipe.write, INVALID_HANDLE_VALUE);
        assert!(!pipe.read.is_null());
        assert!(!pipe.write.is_null());
        close_handle_if_valid(pipe.read);
        close_handle_if_valid(pipe.write);
    }

    #[test]
    fn resolve_stdio_input_null() {
        let (child, parent) = resolve_stdio_input(SandboxStdio::Null).unwrap();
        assert_eq!(child, INVALID_HANDLE_VALUE);
        assert!(parent.is_none());
    }

    #[test]
    fn resolve_stdio_input_piped() {
        let (child, parent) = resolve_stdio_input(SandboxStdio::Piped).unwrap();
        assert_ne!(child, INVALID_HANDLE_VALUE);
        assert!(parent.is_some());
        close_handle_if_valid(child);
        close_optional_handle(parent);
    }

    #[test]
    fn stdio_pipes_close_all() {
        let p1 = create_pipe().unwrap();
        let p2 = create_pipe().unwrap();
        let p3 = create_pipe().unwrap();

        let pipes = StdioPipes {
            child_stdin: p1.read,
            parent_stdin: Some(p1.write),
            child_stdout: p2.write,
            parent_stdout: Some(p2.read),
            child_stderr: p3.write,
            parent_stderr: Some(p3.read),
        };

        // Smoke test: close_all runs without panic and exercises all six handle closes.
        pipes.close_all();
    }
}
