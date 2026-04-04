#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::{FALSE, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::OwnedHandle;

/// Check whether the current process is elevated (running as administrator).
pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();

    // SAFETY: Opening the current process token for query access.
    let ret = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if ret == FALSE {
        return false;
    }

    let token = OwnedHandle(token);

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut return_len: u32 = 0;

    // SAFETY: Querying token elevation status. Buffer is correctly sized.
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&raw mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut return_len,
        )
    };

    ret != FALSE && elevation.TokenIsElevated != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_elevated_does_not_panic() {
        // Just verify it returns a bool without panicking.
        let _result: bool = is_elevated();
    }

    #[test]
    fn is_elevated_deterministic() {
        let first = is_elevated();
        let second = is_elevated();
        assert_eq!(first, second, "should be deterministic between calls");
    }
}
