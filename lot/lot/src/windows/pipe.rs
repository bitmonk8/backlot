#![allow(unsafe_code)]

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
use windows_sys::Win32::System::Pipes::CreatePipe;

use crate::command::SandboxStdio;

use super::{FILE_GENERIC_READ, FILE_GENERIC_WRITE, to_wide};

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

/// Open `\\.\NUL` with inheritable handle for use as child stdio.
fn open_nul_device(read: bool) -> io::Result<HANDLE> {
    let nul_wide = to_wide("\\\\.\\NUL");
    let access = if read {
        FILE_GENERIC_READ
    } else {
        FILE_GENERIC_WRITE
    };

    #[allow(clippy::cast_possible_truncation)]
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: TRUE,
    };

    // SAFETY: Opening the NUL device with an inheritable handle. The wide
    // string is valid for the duration of the call. SECURITY_ATTRIBUTES
    // enables inheritance so the child process can use this handle.
    let handle = unsafe {
        CreateFileW(
            nul_wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &raw mut sa,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(handle)
}

pub fn resolve_stdio_input(spec: SandboxStdio) -> io::Result<(HANDLE, Option<HANDLE>)> {
    match spec {
        SandboxStdio::Null => {
            let handle = open_nul_device(true)?;
            Ok((handle, None))
        }
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
        SandboxStdio::Null => {
            let handle = open_nul_device(false)?;
            Ok((handle, None))
        }
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
    /// Close only owned handles. Parent handles are always owned (from
    /// `CreatePipe`) so they are always closed. Child handles are only
    /// closed for `Piped` and `Null` streams (both owned by us). `Inherit`
    /// child handles are borrowed from the parent process via `GetStdHandle`
    /// and must NOT be closed.
    pub fn close_owned(
        &self,
        stdin_spec: SandboxStdio,
        stdout_spec: SandboxStdio,
        stderr_spec: SandboxStdio,
    ) {
        close_optional_handle(self.parent_stdin);
        close_optional_handle(self.parent_stdout);
        close_optional_handle(self.parent_stderr);

        if stdin_spec != SandboxStdio::Inherit {
            close_handle_if_valid(self.child_stdin);
        }
        if stdout_spec != SandboxStdio::Inherit {
            close_handle_if_valid(self.child_stdout);
        }
        if stderr_spec != SandboxStdio::Inherit {
            close_handle_if_valid(self.child_stderr);
        }
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
        // Null now opens a real handle to \\.\NUL instead of INVALID_HANDLE_VALUE.
        assert_ne!(child, INVALID_HANDLE_VALUE);
        assert!(!child.is_null());
        assert!(parent.is_none());
        close_handle_if_valid(child);
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
    fn stdio_pipes_close_owned_piped() {
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

        // All Piped: close_owned closes all six handles.
        pipes.close_owned(
            SandboxStdio::Piped,
            SandboxStdio::Piped,
            SandboxStdio::Piped,
        );
    }

    #[test]
    fn stdio_pipes_close_owned_skips_inherit() {
        use windows_sys::Win32::System::Console::GetStdHandle;
        use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;

        let p1 = create_pipe().unwrap();
        // SAFETY: Querying stdout handle for test purposes.
        let inherited_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let p3 = create_pipe().unwrap();

        let pipes = StdioPipes {
            child_stdin: p1.read,
            parent_stdin: Some(p1.write),
            child_stdout: inherited_handle,
            parent_stdout: None,
            child_stderr: p3.write,
            parent_stderr: Some(p3.read),
        };

        // Inherit for stdout: close_owned must NOT close the inherited handle.
        pipes.close_owned(
            SandboxStdio::Piped,
            SandboxStdio::Inherit,
            SandboxStdio::Piped,
        );
    }
}
