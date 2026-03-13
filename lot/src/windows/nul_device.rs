#![allow(unsafe_code)]

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, EXPLICIT_ACCESS_W, GRANT_ACCESS,
    GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AllocateAndInitializeSid,
    DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetAce, GetAclInformation,
    GetSecurityDescriptorDacl, GetTokenInformation, PSECURITY_DESCRIPTOR, PSID,
    SID_IDENTIFIER_AUTHORITY, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::error::SandboxError;

const NUL_DEVICE: &str = "\\\\.\\NUL";

const SECURITY_APP_PACKAGE_AUTHORITY: SID_IDENTIFIER_AUTHORITY = SID_IDENTIFIER_AUTHORITY {
    Value: [0, 0, 0, 0, 0, 15],
};
const SECURITY_APP_PACKAGE_BASE_RID: u32 = 2;
const SECURITY_APP_PACKAGE_ALL_PACKAGES_RID: u32 = 1;

/// ACCESS_ALLOWED_ACE_TYPE — not exported by windows-sys without extra features.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// Minimum rights for `fs::metadata()` to succeed on a directory.
// FILE_TRAVERSE: required for path traversal (SeChangeNotifyPrivilege bypass).
// SYNCHRONIZE: required by CreateFileW(access=0, FILE_FLAG_BACKUP_SEMANTICS).
// FILE_READ_ATTRIBUTES: required by GetFileAttributesExW (used by fs::metadata).
const FILE_TRAVERSE: u32 = 0x0020;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const TRAVERSE_MASK: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

use super::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Encode a `Path` as a null-terminated UTF-16 string for Win32 APIs.
fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[allow(clippy::cast_possible_wrap)]
fn win32_error_msg(code: u32) -> String {
    io::Error::from_raw_os_error(code as i32).to_string()
}

/// Allocate the ALL APPLICATION PACKAGES SID (`S-1-15-2-1`).
/// Caller must free with `FreeSid`.
fn allocate_app_packages_sid() -> Option<PSID> {
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: Allocating a well-known SID with two sub-authorities.
    let ret = unsafe {
        AllocateAndInitializeSid(
            &SECURITY_APP_PACKAGE_AUTHORITY,
            2,
            SECURITY_APP_PACKAGE_BASE_RID,
            SECURITY_APP_PACKAGE_ALL_PACKAGES_RID,
            0,
            0,
            0,
            0,
            0,
            0,
            &raw mut sid,
        )
    };
    if ret == FALSE { None } else { Some(sid) }
}

// ---------------------------------------------------------------------------
// NUL device functions
// ---------------------------------------------------------------------------

/// Test whether AppContainer processes can access `\\.\NUL`.
///
/// Returns `true` if either:
/// - The DACL is NULL (unrestricted access — everyone can access the device), or
/// - The DACL contains an allow ACE for ALL APPLICATION PACKAGES (`S-1-15-2-1`).
fn nul_device_accessible() -> bool {
    let wide = to_wide(NUL_DEVICE);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading the DACL of the NUL device. sd freed with LocalFree below.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return false;
    }

    // NULL DACL means unrestricted access — no modification needed.
    let result = has_null_dacl(sd) || sd_contains_app_packages_ace(sd);

    // SAFETY: sd allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd.cast());
    }

    result
}

/// Check whether a security descriptor has a NULL DACL (unrestricted access).
fn has_null_dacl(sd: PSECURITY_DESCRIPTOR) -> bool {
    let mut dacl_present: i32 = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted: i32 = 0;

    // SAFETY: Querying DACL presence from a valid security descriptor.
    let ret = unsafe {
        GetSecurityDescriptorDacl(sd, &raw mut dacl_present, &raw mut dacl, &raw mut defaulted)
    };
    if ret == FALSE {
        return false;
    }

    // dacl_present != 0 but dacl is NULL -> NULL DACL (all access granted).
    dacl_present != 0 && dacl.is_null()
}

/// Convert a security descriptor's DACL to SDDL and check for an allow ACE
/// whose trustee is ALL APPLICATION PACKAGES (SDDL abbreviation: "AC").
fn sd_contains_app_packages_ace(sd: PSECURITY_DESCRIPTOR) -> bool {
    let mut sddl_ptr: *mut u16 = std::ptr::null_mut();

    // SAFETY: Converting a valid security descriptor to SDDL string.
    // Length output parameter is null — we walk the null terminator instead.
    let ret = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &raw mut sddl_ptr,
            std::ptr::null_mut(),
        )
    };
    if ret == FALSE {
        return false;
    }

    // Walk the null-terminated wide string to find its length. The `sddl_len`
    // output parameter has ambiguous documentation (includes vs excludes null),
    // so we measure directly to avoid off-by-one.
    // SAFETY: sddl_ptr is a null-terminated wide string allocated by the conversion function.
    let len = unsafe {
        let mut p = sddl_ptr;
        while *p != 0 {
            p = p.add(1);
        }
        p.offset_from(sddl_ptr) as usize
    };
    let sddl_slice = unsafe { std::slice::from_raw_parts(sddl_ptr, len) };
    let sddl = String::from_utf16_lossy(sddl_slice);

    // SAFETY: sddl_ptr allocated by the conversion function.
    unsafe {
        LocalFree(sddl_ptr.cast());
    }

    sddl_has_ac_allow(&sddl)
}

/// Check whether an SDDL string contains an allow ACE for ALL APPLICATION
/// PACKAGES (SDDL abbreviation: "AC").
///
/// Detects whether we previously added an allow ACE — does NOT model actual
/// DACL evaluation order (a preceding deny ACE would still block access).
///
/// ACE format: `(type;flags;rights;object_guid;inherit_object_guid;account_sid)`
fn sddl_has_ac_allow(sddl: &str) -> bool {
    sddl.split('(')
        .any(|ace| ace.starts_with("A;") && ace.contains(";;;AC)"))
}

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

/// Grant ALL APPLICATION PACKAGES (`S-1-15-2-1`) read/write access to `\\.\NUL`.
fn grant_nul_device() -> crate::Result<()> {
    if nul_device_accessible() {
        return Ok(());
    }

    let wide = to_wide(NUL_DEVICE);
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading the current DACL of the NUL device.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut current_dacl,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to read NUL device DACL: {}",
            win32_error_msg(err),
        )));
    }

    let Some(app_sid) = allocate_app_packages_sid() else {
        // SAFETY: sd allocated by GetNamedSecurityInfoW.
        unsafe {
            LocalFree(sd.cast());
        }
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    };

    let result = apply_nul_dacl(&wide, current_dacl, app_sid);

    // SAFETY: Freeing resources allocated above.
    unsafe {
        FreeSid(app_sid);
        LocalFree(sd.cast());
    }

    result
}

/// Build a new DACL with the ALL APPLICATION PACKAGES ACE and apply it
/// to the NUL device.
fn apply_nul_dacl(wide_path: &[u16], current_dacl: *mut ACL, app_sid: PSID) -> crate::Result<()> {
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
        ptstrName: app_sid.cast(),
    };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        grfAccessMode: GRANT_ACCESS,
        // Device objects have no children — no inheritance needed.
        grfInheritance: 0,
        Trustee: trustee,
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging a new ACE into the existing DACL.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, current_dacl, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build NUL device DACL: {}",
            win32_error_msg(err),
        )));
    }

    // SAFETY: Applying the new DACL to the NUL device.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null(),
        )
    };

    // SAFETY: new_dacl allocated by SetEntriesInAclW.
    unsafe {
        LocalFree(new_dacl.cast());
    }

    if err != ERROR_SUCCESS {
        if err == ERROR_ACCESS_DENIED {
            return Err(SandboxError::Setup(
                "cannot modify NUL device DACL: elevation (run as administrator) required".into(),
            ));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply NUL device DACL: {}",
            win32_error_msg(err),
        )));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Ancestor traverse ACE functions
// ---------------------------------------------------------------------------

/// Walk parents of each path up to the volume root, deduplicate.
/// Does NOT include the paths themselves — only their ancestors.
fn compute_ancestors(paths: &[&Path]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for path in paths {
        // Canonicalize to resolve relative paths and symlinks.
        let Ok(canonical) = std::fs::canonicalize(path) else {
            continue;
        };
        let mut current = canonical.as_path();
        while let Some(parent) = current.parent() {
            // Stop when parent() returns the same path (volume root's parent is itself
            // on some implementations) or an empty path.
            if parent == current {
                break;
            }
            if seen.insert(parent.to_path_buf()) {
                result.push(parent.to_path_buf());
            }
            current = parent;
        }
    }

    result
}

/// Check if a directory's DACL has an allow ACE for ALL APPLICATION PACKAGES
/// with at least `FILE_TRAVERSE | SYNCHRONIZE`.
fn has_traverse_ace(path: &Path) -> bool {
    let wide = path_to_wide(path);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut dacl_ptr: *mut ACL = std::ptr::null_mut();

    // SAFETY: Reading the DACL. dacl_ptr points into sd's memory — only sd needs freeing.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut dacl_ptr,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return false;
    }

    let result = dacl_has_traverse_ace_for_app_packages(dacl_ptr);

    // SAFETY: sd allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd.cast());
    }

    result
}

/// Iterate ACEs in a DACL looking for an ACCESS_ALLOWED_ACE whose SID matches
/// ALL APPLICATION PACKAGES and whose mask includes `FILE_TRAVERSE | SYNCHRONIZE`.
fn dacl_has_traverse_ace_for_app_packages(dacl: *mut ACL) -> bool {
    if dacl.is_null() {
        // NULL DACL = unrestricted access, traverse is implicitly granted.
        return true;
    }

    let Some(app_sid) = allocate_app_packages_sid() else {
        return false;
    };

    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };

    // SAFETY: Querying ACL size info from a valid DACL.
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ret == FALSE {
        // SAFETY: SID from allocate_app_packages_sid.
        unsafe {
            FreeSid(app_sid);
        }
        return false;
    }

    let mut found = false;
    for i in 0..info.AceCount {
        let mut ace_ptr: *mut std::ffi::c_void = std::ptr::null_mut();

        // SAFETY: Reading ACE at valid index from a valid DACL.
        let ret = unsafe { GetAce(dacl, i, &raw mut ace_ptr) };
        if ret == FALSE {
            continue;
        }

        // SAFETY: ace_ptr points to a valid ACE header within the DACL buffer.
        let ace = unsafe { &*(ace_ptr.cast::<ACCESS_ALLOWED_ACE>()) };
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE {
            continue;
        }

        // The SID starts at SidStart field offset within the ACE struct.
        let sid_ptr: PSID = (&raw const ace.SidStart).cast_mut().cast();

        // SAFETY: Both SIDs are valid — one from the ACE, one from AllocateAndInitializeSid.
        let sids_equal = unsafe { EqualSid(sid_ptr, app_sid) } != FALSE;
        if sids_equal && (ace.Mask & TRAVERSE_MASK) == TRAVERSE_MASK {
            found = true;
            break;
        }
    }

    // SAFETY: SID from allocate_app_packages_sid.
    unsafe {
        FreeSid(app_sid);
    }

    found
}

/// Grant `FILE_TRAVERSE | SYNCHRONIZE` with `NO_INHERITANCE` for
/// ALL APPLICATION PACKAGES on a single directory.
fn grant_traverse(path: &Path) -> crate::Result<()> {
    if has_traverse_ace(path) {
        return Ok(());
    }

    let wide = path_to_wide(path);
    let mut current_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();

    // SAFETY: Reading the current DACL of the directory.
    let err = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut current_dacl,
            std::ptr::null_mut(),
            &raw mut sd,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to read DACL for {}: {}",
            path.display(),
            win32_error_msg(err),
        )));
    }

    let Some(app_sid) = allocate_app_packages_sid() else {
        // SAFETY: sd allocated by GetNamedSecurityInfoW.
        unsafe {
            LocalFree(sd.cast());
        }
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    };

    let result = apply_traverse_dacl(&wide, path, current_dacl, app_sid);

    // SAFETY: Freeing resources allocated above.
    unsafe {
        FreeSid(app_sid);
        LocalFree(sd.cast());
    }

    result
}

/// Build a new DACL with the traverse ACE and apply it to the directory.
fn apply_traverse_dacl(
    wide_path: &[u16],
    display_path: &Path,
    current_dacl: *mut ACL,
    app_sid: PSID,
) -> crate::Result<()> {
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
        ptstrName: app_sid.cast(),
    };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: TRAVERSE_MASK,
        grfAccessMode: GRANT_ACCESS,
        // Ancestors only — children are covered by policy path ACEs.
        grfInheritance: 0,
        Trustee: trustee,
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging a new ACE into the existing DACL.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, current_dacl, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build traverse DACL for {}: {}",
            display_path.display(),
            win32_error_msg(err),
        )));
    }

    // SAFETY: Applying the new DACL to the directory.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr().cast_mut(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null(),
        )
    };
    // SAFETY: new_dacl allocated by SetEntriesInAclW.
    unsafe {
        LocalFree(new_dacl.cast());
    }

    if err != ERROR_SUCCESS {
        if err == ERROR_ACCESS_DENIED {
            return Err(SandboxError::Setup(format!(
                "cannot modify DACL for {}: elevation (run as administrator) required",
                display_path.display(),
            )));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply traverse DACL for {}: {}",
            display_path.display(),
            win32_error_msg(err),
        )));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// One-time elevated setup. Grants all ACEs needed for AppContainer sandboxes
/// to function correctly on Windows:
///   1. NUL device read/write for ALL APPLICATION PACKAGES
///   2. Traverse ACEs on each ancestor of the provided paths, up to (and
///      including) the volume root
///
/// Idempotent — safe to call multiple times. Requires elevation.
pub fn grant_appcontainer_prerequisites(paths: &[&Path]) -> crate::Result<()> {
    grant_nul_device()?;

    let ancestors = compute_ancestors(paths);
    for ancestor in &ancestors {
        grant_traverse(ancestor)?;
    }

    Ok(())
}

/// Checks whether all ancestors of each path (up to volume root) have the
/// ALL APPLICATION PACKAGES traverse ACE, and the NUL device ACE exists.
pub fn appcontainer_prerequisites_met(paths: &[&Path]) -> bool {
    if !nul_device_accessible() {
        return false;
    }

    let ancestors = compute_ancestors(paths);
    ancestors.iter().all(|a| has_traverse_ace(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_device_accessible_returns_bool() {
        // Verify it runs without panic and returns a definite value.
        let result: bool = nul_device_accessible();
        let _ = result;
    }

    #[test]
    fn is_elevated_returns_bool() {
        // CI runners and dev machines are typically non-elevated.
        if !is_elevated() {
            assert!(!is_elevated(), "should be deterministic");
        }
    }

    #[test]
    fn appcontainer_prerequisites_met_empty_paths() {
        // With no paths, only checks NUL device — must not panic.
        let result: bool = appcontainer_prerequisites_met(&[]);
        let _ = result;
    }

    #[test]
    fn compute_ancestors_empty_input() {
        let result = compute_ancestors(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn compute_ancestors_single_path() {
        // Use a path known to exist on all Windows machines.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = PathBuf::from(format!("{system_root}\\System32"));
        let path_ref: &Path = &path;
        let ancestors = compute_ancestors(&[path_ref]);

        // Should contain at least the system root and the volume root.
        assert!(
            ancestors.len() >= 2,
            "expected at least 2 ancestors, got {}: {:?}",
            ancestors.len(),
            ancestors
        );

        // Should NOT contain the path itself.
        let canonical = std::fs::canonicalize(&path).ok();
        if let Some(ref c) = canonical {
            assert!(
                !ancestors.contains(c),
                "ancestors should not contain the input path itself"
            );
        }
    }

    #[test]
    fn compute_ancestors_deduplicates() {
        // Two paths under the same parent should not produce duplicate ancestors.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let p1 = PathBuf::from(format!("{system_root}\\System32"));
        let p2 = PathBuf::from(format!("{system_root}\\Temp"));
        let p1_ref: &Path = &p1;
        let p2_ref: &Path = &p2;
        let ancestors = compute_ancestors(&[p1_ref, p2_ref]);

        // Check no duplicates.
        let mut seen = std::collections::HashSet::new();
        for a in &ancestors {
            assert!(seen.insert(a), "duplicate ancestor: {}", a.display());
        }
    }

    #[test]
    fn sddl_parsing_detects_ac() {
        assert!(sddl_has_ac_allow(
            "D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x12019f;;;AC)"
        ));
    }

    #[test]
    fn sddl_parsing_rejects_missing_ac() {
        assert!(!sddl_has_ac_allow("D:(A;;FA;;;SY)(A;;FA;;;BA)"));
    }

    #[test]
    fn sddl_parsing_rejects_deny_ace_for_ac() {
        assert!(!sddl_has_ac_allow("D:(D;;FA;;;AC)"));
    }

    #[test]
    fn sddl_parsing_empty_inputs() {
        assert!(!sddl_has_ac_allow(""));
        assert!(!sddl_has_ac_allow("D:"));
    }

    #[test]
    fn sddl_parsing_deny_then_allow_for_ac() {
        // Both deny and allow ACEs for AC — function detects the allow ACE.
        // Actual DACL evaluation is order-dependent (deny wins), but our
        // check only needs to detect whether we previously added an allow ACE.
        assert!(sddl_has_ac_allow("D:(D;;FA;;;AC)(A;;0x12019f;;;AC)"));
    }

    #[test]
    fn sddl_parsing_rejects_ac_substring_trustee() {
        // "SOMEAC" ends with "AC" but is not the AC SID — must not match.
        assert!(!sddl_has_ac_allow("D:(A;;FA;;;SOMEAC)"));
    }

    #[test]
    fn sddl_parsing_rejects_audit_ace_type() {
        // "AU;" (audit) starts with 'A' but is not "A;" (allow).
        assert!(!sddl_has_ac_allow("D:(AU;;FA;;;AC)"));
    }
}
