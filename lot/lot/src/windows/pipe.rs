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

impl PipeHandles {
    /// Extract both handles, consuming self without running Drop.
    pub fn into_parts(self) -> (HANDLE, HANDLE) {
        let this = std::mem::ManuallyDrop::new(self);
        (this.read, this.write)
    }
}

impl Drop for PipeHandles {
    fn drop(&mut self) {
        close_handle_if_valid(self.read);
        close_handle_if_valid(self.write);
    }
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
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            if handle.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no stdin handle available",
                ));
            }
            Ok((handle, None))
        }
        SandboxStdio::Piped => {
            let (read, write) = create_pipe()?.into_parts();
            // Child reads from read end, parent writes to write end.
            Ok((read, Some(write)))
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
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            if handle.is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no stdout/stderr handle available",
                ));
            }
            Ok((handle, None))
        }
        SandboxStdio::Piped => {
            let (read, write) = create_pipe()?.into_parts();
            // Child writes to write end, parent reads from read end.
            Ok((write, Some(read)))
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
/// Tracks which child handles are owned (Piped/Null) vs borrowed (Inherit)
/// so Drop can release resources without leaking or double-closing.
pub struct StdioPipes {
    pub child_stdin: HANDLE,
    pub parent_stdin: Option<HANDLE>,
    pub child_stdout: HANDLE,
    pub parent_stdout: Option<HANDLE>,
    pub child_stderr: HANDLE,
    pub parent_stderr: Option<HANDLE>,
    // Tracks which child handles are owned and must be closed on drop.
    pub(super) stdin_owned: bool,
    pub(super) stdout_owned: bool,
    pub(super) stderr_owned: bool,
}

impl StdioPipes {
    /// Create a new `StdioPipes` with ownership tracking.
    /// Child handles from `Piped` or `Null` are owned; `Inherit` handles are borrowed.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        child_stdin: HANDLE,
        parent_stdin: Option<HANDLE>,
        child_stdout: HANDLE,
        parent_stdout: Option<HANDLE>,
        child_stderr: HANDLE,
        parent_stderr: Option<HANDLE>,
        stdin_spec: SandboxStdio,
        stdout_spec: SandboxStdio,
        stderr_spec: SandboxStdio,
    ) -> Self {
        Self {
            child_stdin,
            parent_stdin,
            child_stdout,
            parent_stdout,
            child_stderr,
            parent_stderr,
            stdin_owned: stdin_spec != SandboxStdio::Inherit,
            stdout_owned: stdout_spec != SandboxStdio::Inherit,
            stderr_owned: stderr_spec != SandboxStdio::Inherit,
        }
    }
}

impl Drop for StdioPipes {
    fn drop(&mut self) {
        close_optional_handle(self.parent_stdin.take());
        close_optional_handle(self.parent_stdout.take());
        close_optional_handle(self.parent_stderr.take());

        if self.stdin_owned {
            close_handle_if_valid(self.child_stdin);
        }
        if self.stdout_owned {
            close_handle_if_valid(self.child_stdout);
        }
        if self.stderr_owned {
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
        let (read, write) = create_pipe().unwrap().into_parts();
        assert_ne!(read, INVALID_HANDLE_VALUE);
        assert_ne!(write, INVALID_HANDLE_VALUE);
        assert!(!read.is_null());
        assert!(!write.is_null());
        close_handle_if_valid(read);
        close_handle_if_valid(write);
    }

    #[test]
    fn resolve_stdio_input_null() {
        let (child, parent) = resolve_stdio_input(SandboxStdio::Null).unwrap();
        // NUL device yields a real, valid handle.
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
    fn stdio_pipes_drop_closes_piped() {
        let (r1, w1) = create_pipe().unwrap().into_parts();
        let (r2, w2) = create_pipe().unwrap().into_parts();
        let (r3, w3) = create_pipe().unwrap().into_parts();

        let pipes = StdioPipes::new(
            r1,
            Some(w1),
            w2,
            Some(r2),
            w3,
            Some(r3),
            SandboxStdio::Piped,
            SandboxStdio::Piped,
            SandboxStdio::Piped,
        );

        // All Piped: Drop closes all six handles.
        drop(pipes);
    }

    #[test]
    fn stdio_pipes_drop_skips_inherit() {
        use windows_sys::Win32::Storage::FileSystem::GetFileType;
        use windows_sys::Win32::System::Console::GetStdHandle;
        use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;

        let (r1, w1) = create_pipe().unwrap().into_parts();
        // SAFETY: Querying stdout handle for test purposes.
        let inherited_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let (r3, w3) = create_pipe().unwrap().into_parts();

        let pipes = StdioPipes::new(
            r1,
            Some(w1),
            inherited_handle,
            None,
            w3,
            Some(r3),
            SandboxStdio::Piped,
            SandboxStdio::Inherit,
            SandboxStdio::Piped,
        );

        // Inherit for stdout: Drop must NOT close the inherited handle.
        drop(pipes);

        // Verify the inherited handle is still valid after Drop.
        // SAFETY: GetFileType on a handle we know came from GetStdHandle.
        let file_type = unsafe { GetFileType(inherited_handle) };
        // FILE_TYPE_UNKNOWN with no error means closed handle; any other type means valid.
        assert_ne!(
            file_type, 0,
            "inherited stdout handle should still be valid after Drop"
        );
    }

    #[test]
    fn resolve_stdio_output_piped() {
        use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;

        let (child, parent) = resolve_stdio_output(SandboxStdio::Piped, STD_OUTPUT_HANDLE).unwrap();
        assert_ne!(child, INVALID_HANDLE_VALUE);
        assert!(!child.is_null());
        assert!(parent.is_some(), "Piped mode should have a parent handle");
        let parent_handle = parent.unwrap();
        assert_ne!(parent_handle, INVALID_HANDLE_VALUE);
        assert!(!parent_handle.is_null());
        // Child gets write end, parent gets read end.
        close_handle_if_valid(child);
        close_handle_if_valid(parent_handle);
    }

    #[test]
    fn resolve_stdio_output_null() {
        use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;

        let (child, parent) = resolve_stdio_output(SandboxStdio::Null, STD_OUTPUT_HANDLE).unwrap();
        assert_ne!(child, INVALID_HANDLE_VALUE);
        assert!(!child.is_null());
        assert!(parent.is_none(), "Null mode should have no parent handle");
        close_handle_if_valid(child);
    }
}
