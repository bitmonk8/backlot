#![allow(unsafe_code)]

//! Directory traverse ACE utilities for ALL APPLICATION PACKAGES.
//!
//! Grants and checks `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`
//! ACEs on directory ancestors so AppContainer sandboxed processes can walk
//! path components via `fs::metadata()`.

use std::path::{Path, PathBuf};

/// Marker substring embedded in `SandboxError::Setup` messages when the failure
/// is due to insufficient privilege (ACCESS_DENIED). Used by `appcontainer.rs`
/// to distinguish prerequisite failures from transient I/O errors.
pub const ELEVATION_REQUIRED_MARKER: &str = "elevation required";

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, FALSE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    EqualSid, FreeSid, GetAce, GetAclInformation, PSECURITY_DESCRIPTOR, PSID,
};

use super::acl_helpers::allocate_app_packages_sid;
use crate::error::SandboxError;

/// ACCESS_ALLOWED_ACE_TYPE — not exported by windows-sys without extra features.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// Minimum rights for `fs::metadata()` to succeed on a directory.
const FILE_TRAVERSE: u32 = 0x0020;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const TRAVERSE_MASK: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

use super::path_to_wide;

/// Walk parents of each path up to the volume root, deduplicate.
/// Does NOT include the paths themselves — only their ancestors.
///
/// Returns an error if any path cannot be canonicalized, preventing
/// vacuous-truth checks when all paths are silently skipped.
pub fn compute_ancestors<P: AsRef<Path>>(
    paths: &[P],
) -> std::result::Result<Vec<PathBuf>, SandboxError> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for path in paths {
        let canonical = std::fs::canonicalize(path.as_ref()).map_err(|e| {
            SandboxError::Setup(format!(
                "failed to canonicalize path {}: {e}",
                path.as_ref().display()
            ))
        })?;
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

    Ok(result)
}

/// Check if a directory's DACL has an allow ACE for ALL APPLICATION PACKAGES
/// with at least `TRAVERSE_MASK` (`FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`).
pub fn has_traverse_ace(path: &Path) -> bool {
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
/// ALL APPLICATION PACKAGES and whose mask includes `TRAVERSE_MASK`.
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

/// Grant `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES` with
/// `NO_INHERITANCE` for ALL APPLICATION PACKAGES on a single directory.
///
/// Reads the DACL once and checks for the ACE in the same read to avoid
/// the TOCTOU race of separate check-then-modify calls. If the ACE already
/// exists, no write is attempted. `SetEntriesInAclW` is idempotent, so
/// duplicate ACEs from a concurrent modifier are harmless.
pub fn grant_traverse(path: &Path) -> crate::Result<()> {
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
        return Err(SandboxError::Setup(format!(
            "failed to read DACL for {}: {}",
            path.display(),
            super::win32_error_msg(err),
        )));
    }

    // Check in the same DACL read whether the ACE already exists.
    if dacl_has_traverse_ace_for_app_packages(dacl_ptr) {
        // SAFETY: sd allocated by GetNamedSecurityInfoW.
        unsafe {
            LocalFree(sd.cast());
        }
        return Ok(());
    }

    // SAFETY: sd allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(sd.cast());
    }

    // ACE not present — apply it via the shared helper.
    let Some(app_sid) = allocate_app_packages_sid() else {
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    };

    // Ancestors only — children are covered by policy path ACEs (no inheritance).
    let result = super::acl_helpers::apply_dacl(&wide, Some(path), TRAVERSE_MASK, 0, app_sid);

    // SAFETY: SID from allocate_app_packages_sid.
    unsafe {
        FreeSid(app_sid);
    }

    // Translate the generic "elevation required" message to include our marker
    // so appcontainer.rs can distinguish prerequisite failures from I/O errors.
    result.map_err(|e| {
        if let SandboxError::Setup(ref msg) = e {
            if msg.contains("elevation required") {
                return SandboxError::Setup(format!(
                    "cannot modify DACL for {}: {} (run as administrator)",
                    path.display(),
                    ELEVATION_REQUIRED_MARKER,
                ));
            }
        }
        e
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn compute_ancestors_empty_input() {
        let result = compute_ancestors::<&Path>(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn compute_ancestors_single_path() {
        // Use a path known to exist on all Windows machines.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let path = PathBuf::from(format!("{system_root}\\System32"));
        let ancestors = compute_ancestors(&[path.as_path()]).unwrap();

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
        let ancestors = compute_ancestors(&[p1.as_path(), p2.as_path()]).unwrap();

        // Check no duplicates.
        let mut seen = std::collections::HashSet::new();
        for a in &ancestors {
            assert!(seen.insert(a), "duplicate ancestor: {}", a.display());
        }
    }

    #[test]
    fn compute_ancestors_rejects_nonexistent_path() {
        let result =
            compute_ancestors(&[Path::new(r"C:\This\Path\Definitely\Does\Not\Exist\12345")]);
        assert!(result.is_err(), "non-existent path should produce an error");
    }

    #[test]
    fn compute_ancestors_root_path() {
        // Volume root should produce no ancestors (it has no parent).
        let ancestors = compute_ancestors(&[Path::new(r"C:\")]).unwrap();
        assert!(
            ancestors.is_empty(),
            "root path should have no ancestors: {ancestors:?}",
        );
    }

    #[test]
    fn compute_ancestors_deeply_nested() {
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        // System32\drivers is typically 3+ levels deep from the volume root.
        let path = PathBuf::from(format!("{system_root}\\System32\\drivers"));
        if path.exists() {
            let ancestors = compute_ancestors(&[path.as_path()]).unwrap();
            assert!(
                ancestors.len() >= 3,
                "deeply nested path should have >= 3 ancestors, got {}: {:?}",
                ancestors.len(),
                ancestors
            );
        }
    }

    #[test]
    fn has_traverse_ace_system_directory() {
        // C:\Windows is a system directory — just verify the function doesn't
        // panic and returns a bool.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let _result: bool = has_traverse_ace(Path::new(&system_root));
    }
}
