#![allow(unsafe_code)]

//! Directory traverse ACE utilities for ALL APPLICATION PACKAGES.
//!
//! Grants and checks `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`
//! ACEs on directory ancestors so AppContainer sandboxed processes can walk
//! path components via `fs::metadata()`.
//!
//! Uses `SetFileSecurityW` instead of `SetNamedSecurityInfoW` /
//! `SetSecurityInfo` to avoid O(subtree) inheritance propagation on
//! directories with large subtrees (e.g. `C:\Users`). Both
//! `SetNamedSecurityInfoW` and `SetSecurityInfo` re-evaluate
//! auto-inheritance for the entire subtree; `SetFileSecurityW` writes
//! only the target object's DACL.

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, FALSE};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SetEntriesInAclW, TRUSTEE_IS_SID,
    TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, InitializeSecurityDescriptor, SE_DACL_AUTO_INHERITED,
    SECURITY_DESCRIPTOR, SetFileSecurityW, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl,
};

use super::acl_helpers::{
    ELEVATION_REQUIRED_MARKER, OwnedAcl, allocate_app_packages_sid, dacl_has_app_packages_ace,
    read_dacl,
};
use super::{path_to_wide, win32_error_msg};
use crate::error::SandboxError;

/// Minimum rights for `fs::metadata()` to succeed on a directory.
const FILE_TRAVERSE: u32 = 0x0020;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const TRAVERSE_MASK: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

/// Walk parents of each path up to the volume root, deduplicate.
/// Does NOT include the paths themselves -- only their ancestors.
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
    let Some((dacl_ptr, _sd_guard)) = read_dacl(&wide) else {
        return false;
    };
    dacl_has_app_packages_ace(dacl_ptr, TRAVERSE_MASK)
}

/// SECURITY_DESCRIPTOR_REVISION — required by `InitializeSecurityDescriptor`.
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

/// Grant `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES` with
/// `NO_INHERITANCE` for ALL APPLICATION PACKAGES on a single directory.
///
/// Uses `SetFileSecurityW` instead of `SetNamedSecurityInfoW` or
/// `SetSecurityInfo`. Both high-level APIs re-evaluate auto-inheritance
/// for the entire subtree, which takes minutes on directories like
/// `C:\Users` with large subtrees. `SetFileSecurityW` writes only the
/// target object's DACL with no subtree walk.
///
/// Reads the DACL once and checks for the ACE in the same read to avoid
/// the TOCTOU race of separate check-then-modify calls. If the ACE already
/// exists, no write is attempted.
pub fn grant_traverse(path: &Path) -> crate::Result<()> {
    let wide = path_to_wide(path);

    let Some((dacl_ptr, _sd_guard)) = read_dacl(&wide) else {
        return Err(SandboxError::Setup(format!(
            "failed to read DACL for {}: unable to read security info",
            path.display(),
        )));
    };

    // Check in the same DACL read whether the ACE already exists.
    if dacl_has_app_packages_ace(dacl_ptr, TRAVERSE_MASK) {
        return Ok(());
    }

    // ACE not present — build merged DACL and apply via SetFileSecurityW.
    let Some(app_sid) = allocate_app_packages_sid() else {
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    };

    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
        ptstrName: app_sid.as_raw().cast(),
    };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: TRAVERSE_MASK,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0, // Ancestors only — no inheritance.
        Trustee: trustee,
    };

    // Merge the new ACE into the existing DACL.
    let mut new_dacl: *mut ACL = std::ptr::null_mut();

    // SAFETY: Merging one ACE into the existing DACL read above.
    let err = unsafe { SetEntriesInAclW(1, &raw const ea, dacl_ptr, &raw mut new_dacl) };
    if err != ERROR_SUCCESS {
        return Err(SandboxError::Setup(format!(
            "failed to build DACL for {}: {}",
            path.display(),
            win32_error_msg(err),
        )));
    }

    let Some(new_dacl_guard) = OwnedAcl::new(new_dacl) else {
        return Err(SandboxError::Setup(format!(
            "SetEntriesInAclW returned null DACL for {}",
            path.display(),
        )));
    };

    // Build a self-relative security descriptor containing the merged DACL.
    // SetFileSecurityW does NOT trigger inheritance propagation, unlike
    // SetNamedSecurityInfoW and SetSecurityInfo.
    let mut sd = SECURITY_DESCRIPTOR {
        Revision: 0,
        Sbz1: 0,
        Control: 0,
        Owner: std::ptr::null_mut(),
        Group: std::ptr::null_mut(),
        Sacl: std::ptr::null_mut(),
        Dacl: std::ptr::null_mut(),
    };

    // SAFETY: Initializing a stack-allocated security descriptor.
    let ret =
        unsafe { InitializeSecurityDescriptor((&raw mut sd).cast(), SECURITY_DESCRIPTOR_REVISION) };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "InitializeSecurityDescriptor failed for {}",
            path.display(),
        )));
    }

    // SAFETY: Attaching the merged DACL to the security descriptor.
    // The DACL memory (owned by new_dacl_guard) must outlive the
    // SetFileSecurityW call.
    let ret = unsafe {
        SetSecurityDescriptorDacl((&raw mut sd).cast(), 1, new_dacl_guard.as_raw(), FALSE)
    };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "SetSecurityDescriptorDacl failed for {}",
            path.display(),
        )));
    }

    // Mark the DACL as auto-inherited so that SetNamedSecurityInfoW on
    // child objects correctly re-derives inherited ACEs from this directory.
    // Without this flag the DACL is treated as "legacy" and children may
    // lose their inherited ACEs when their DACLs are re-evaluated.
    // SAFETY: Setting a control bit on a valid, initialized security descriptor.
    let ret = unsafe {
        SetSecurityDescriptorControl(
            (&raw mut sd).cast(),
            SE_DACL_AUTO_INHERITED,
            SE_DACL_AUTO_INHERITED,
        )
    };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "SetSecurityDescriptorControl failed for {}",
            path.display(),
        )));
    }

    // SAFETY: Applying the security descriptor to the directory.
    // SetFileSecurityW writes only this object's DACL — no subtree walk.
    let ret = unsafe {
        SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION,
            (&raw mut sd).cast(),
        )
    };

    if ret == FALSE {
        let err = std::io::Error::last_os_error();
        #[allow(clippy::cast_possible_wrap)]
        if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            return Err(SandboxError::Setup(format!(
                "cannot modify DACL for {}: {ELEVATION_REQUIRED_MARKER} (run as administrator)",
                path.display(),
            )));
        }
        return Err(SandboxError::Setup(format!(
            "failed to apply DACL for {}: {err}",
            path.display(),
        )));
    }

    Ok(())
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
    #[ignore = "UNC paths require network share availability"]
    fn compute_ancestors_unc_path() {
        // UNC paths have the form \\server\share\dir. This test is ignored
        // unless a UNC path is guaranteed to exist in the test environment.
        let unc = Path::new(r"\\localhost\C$\Windows\System32");
        let result = compute_ancestors(&[unc]);
        // Just verify it doesn't panic; result depends on environment.
        let _ = result;
    }

    #[test]
    fn compute_ancestors_overlapping_prefix_paths() {
        // Two paths where one is an ancestor of the other should not produce
        // duplicates or incorrect results.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let shallow = PathBuf::from(&system_root);
        let deep = PathBuf::from(format!("{system_root}\\System32"));
        let ancestors = compute_ancestors(&[shallow.as_path(), deep.as_path()]).unwrap();

        // Check no duplicates.
        let mut seen = std::collections::HashSet::new();
        for a in &ancestors {
            assert!(seen.insert(a), "duplicate ancestor: {}", a.display());
        }
    }

    #[test]
    fn compute_ancestors_trailing_backslash() {
        // Paths with trailing backslashes should be handled like their
        // non-trailing equivalents.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let with_slash = PathBuf::from(format!("{system_root}\\System32\\"));
        let without_slash = PathBuf::from(format!("{system_root}\\System32"));

        let a1 = compute_ancestors(&[with_slash.as_path()]).unwrap();
        let a2 = compute_ancestors(&[without_slash.as_path()]).unwrap();

        // Both should produce the same set of ancestors.
        let s1: std::collections::HashSet<_> = a1.into_iter().collect();
        let s2: std::collections::HashSet<_> = a2.into_iter().collect();
        assert_eq!(s1, s2, "trailing backslash should not affect ancestors");
    }

    #[test]
    fn has_traverse_ace_system_directory() {
        // C:\Windows is a system directory -- just verify the function doesn't
        // panic and returns a bool.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let _result: bool = has_traverse_ace(Path::new(&system_root));
    }

    #[test]
    fn has_traverse_ace_null_dacl_returns_true() {
        // Directly test the null-DACL early-return path: a null DACL means
        // unrestricted access, so dacl_has_app_packages_ace should return true.
        assert!(
            dacl_has_app_packages_ace(std::ptr::null_mut(), TRAVERSE_MASK),
            "null DACL should return true (unrestricted access)"
        );
    }
}
