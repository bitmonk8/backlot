#[cfg(windows)]
pub use self::windows::restrict_windows_permissions;

#[cfg(windows)]
#[allow(unsafe_code, clippy::too_many_lines)]
mod windows {
    use crate::error::CredentialError;

    pub fn restrict_windows_permissions(path: &std::path::Path) -> Result<(), CredentialError> {
        use ::windows::Win32::Foundation::{
            CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, LocalFree, WIN32_ERROR,
        };
        use ::windows::Win32::Security::Authorization::{
            EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
            TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
        };
        use ::windows::Win32::Security::{
            ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
            OBJECT_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        };
        use ::windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use ::windows::core::PCWSTR;
        use std::os::windows::ffi::OsStrExt;

        struct HandleGuard(HANDLE);
        impl Drop for HandleGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }

        struct AclGuard(*mut ACL);
        impl Drop for AclGuard {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe {
                        let _ = LocalFree(Some(HLOCAL(self.0.cast())));
                    }
                }
            }
        }

        fn win32_err(context: &str, code: WIN32_ERROR) -> CredentialError {
            CredentialError::InvalidFormat(format!("{context}: error code {}", code.0))
        }

        let token = {
            let mut h = HANDLE::default();
            unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut h) }
                .map_err(|e| CredentialError::InvalidFormat(format!("OpenProcessToken: {e}")))?;
            h
        };
        let _token_guard = HandleGuard(token);

        let mut needed = 0u32;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };

        if needed == 0 {
            return Err(CredentialError::InvalidFormat(
                "GetTokenInformation probe returned size 0".into(),
            ));
        }

        let align_len = (needed as usize).div_ceil(std::mem::size_of::<u64>());
        let mut aligned: Vec<u64> = vec![0u64; align_len];
        let buffer: &mut [u8] =
            unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast(), needed as usize) };
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                needed,
                &raw mut needed,
            )
        }
        .map_err(|e| CredentialError::InvalidFormat(format!("GetTokenInformation: {e}")))?;

        let user_sid: PSID = unsafe { (*aligned.as_ptr().cast::<TOKEN_USER>()).User.Sid };

        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: 0x001F_01FF,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: ::windows::core::PWSTR(user_sid.0.cast()),
                ..Default::default()
            },
        };

        let mut acl_ptr = std::ptr::null_mut::<ACL>();
        let result = unsafe { SetEntriesInAclW(Some(&[ea]), None, &raw mut acl_ptr) };
        if result != ERROR_SUCCESS {
            return Err(win32_err("SetEntriesInAclW", result));
        }
        let _acl_guard = AclGuard(acl_ptr);

        let path_wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let sec_info: OBJECT_SECURITY_INFORMATION =
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;

        let result = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                sec_info,
                None,
                None,
                Some(acl_ptr),
                None,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(win32_err("SetNamedSecurityInfoW", result));
        }

        Ok(())
    }
}
