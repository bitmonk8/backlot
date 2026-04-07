#[cfg(windows)]
pub use self::windows::restrict_windows_permissions;

#[cfg(windows)]
#[allow(unsafe_code, clippy::too_many_lines)]
mod windows {
    use crate::error::CredentialError;

    pub fn restrict_windows_permissions(path: &std::path::Path) -> Result<(), CredentialError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_SUCCESS, FALSE, HANDLE, LocalFree,
        };
        use windows_sys::Win32::Security::Authorization::{
            EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
            SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
        };
        use windows_sys::Win32::Security::{
            ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
            PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        struct HandleGuard(HANDLE);
        impl Drop for HandleGuard {
            fn drop(&mut self) {
                // SAFETY: Closing a valid handle obtained from OpenProcessToken.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }

        struct AclGuard(*mut ACL);
        impl Drop for AclGuard {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    // SAFETY: Freeing memory allocated by SetEntriesInAclW.
                    unsafe {
                        LocalFree(self.0.cast());
                    }
                }
            }
        }

        fn win32_err(context: &str, code: u32) -> CredentialError {
            CredentialError::InvalidFormat(format!("{context}: error code {code}"))
        }

        let token = {
            let mut h: HANDLE = std::ptr::null_mut();
            // SAFETY: Getting the process token for the current process.
            let ret = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut h) };
            if ret == FALSE {
                return Err(CredentialError::InvalidFormat(format!(
                    "OpenProcessToken: error code {}",
                    std::io::Error::last_os_error()
                )));
            }
            h
        };
        let _token_guard = HandleGuard(token);

        let mut needed = 0u32;
        // SAFETY: Probing for required buffer size. Passing null buffer with size 0.
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut needed)
        };

        if needed == 0 {
            return Err(CredentialError::InvalidFormat(
                "GetTokenInformation probe returned size 0".into(),
            ));
        }

        let align_len = (needed as usize).div_ceil(std::mem::size_of::<u64>());
        let mut aligned: Vec<u64> = vec![0u64; align_len];
        let buffer: &mut [u8] =
            // SAFETY: Reinterpreting aligned u64 buffer as u8 slice for Win32 call.
            unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast(), needed as usize) };
        // SAFETY: Querying token user info into a properly sized, aligned buffer.
        let ret = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };
        if ret == FALSE {
            return Err(CredentialError::InvalidFormat(format!(
                "GetTokenInformation: error code {}",
                std::io::Error::last_os_error()
            )));
        }

        // SAFETY: Buffer contains a valid TOKEN_USER after successful GetTokenInformation.
        let user_sid: PSID = unsafe { (*aligned.as_ptr().cast::<TOKEN_USER>()).User.Sid };

        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: 0x001F_01FF,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: user_sid.cast(),
            },
        };

        let mut acl_ptr = std::ptr::null_mut::<ACL>();
        // SAFETY: Building a new ACL with one explicit access entry.
        let result = unsafe { SetEntriesInAclW(1, &ea, std::ptr::null_mut(), &raw mut acl_ptr) };
        if result != ERROR_SUCCESS {
            return Err(win32_err("SetEntriesInAclW", result));
        }
        let _acl_guard = AclGuard(acl_ptr);

        let path_wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let sec_info: u32 = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;

        // SAFETY: Applying the new DACL to the file at path_wide (null-terminated UTF-16).
        // Null pointers for owner/group/SACL — we only set the DACL.
        let result = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr().cast_mut(),
                SE_FILE_OBJECT,
                sec_info,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl_ptr,
                std::ptr::null_mut(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(win32_err("SetNamedSecurityInfoW", result));
        }

        Ok(())
    }
}
