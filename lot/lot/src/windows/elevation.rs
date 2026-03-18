#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Check whether the current process is elevated (running as administrator).
pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();

    // SAFETY: Opening the current process token for query access.
    let ret = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if ret == FALSE {
        return false;
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut return_len: u32 = 0;

    // SAFETY: Querying token elevation status. Buffer is correctly sized.
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut return_len,
        )
    };

    // SAFETY: Token handle from OpenProcessToken.
    unsafe {
        CloseHandle(token);
    }

    ret != FALSE && elevation.TokenIsElevated != 0
}
