#![allow(unsafe_code)]

//! Directory traverse ACE utilities for ALL APPLICATION PACKAGES.
//!
//! Grants and checks `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES`
//! ACEs on directory ancestors so AppContainer sandboxed processes can walk
//! path components via `fs::metadata()`.
//!
//! Uses `NtSetSecurityObject` (kernel API via ntdll) instead of
//! `SetNamedSecurityInfoW` / `SetSecurityInfo` to avoid O(subtree)
//! inheritance propagation on directories with large subtrees (e.g.
//! `C:\Users`). The user-mode APIs re-evaluate auto-inheritance for
//! the entire subtree; the kernel API writes only the target object's
//! security descriptor without any propagation.

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, FALSE, HANDLE};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION_INFORMATION, ACL_SIZE_INFORMATION, AclRevisionInformation,
    AclSizeInformation, AddAccessAllowedAceEx, AddAce, DACL_SECURITY_INFORMATION,
    GetAclInformation, GetSecurityDescriptorControl, InitializeAcl, InitializeSecurityDescriptor,
    SECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR_CONTROL, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, OPEN_EXISTING,
};

use super::acl_helpers::{
    ELEVATION_REQUIRED_MARKER, allocate_app_packages_sid, dacl_has_ace_for_sid,
    dacl_has_app_packages_ace, read_dacl,
};
use super::path_to_wide;
use crate::error::SandboxError;

// Raw FFI to ntdll!NtSetSecurityObject — the kernel API that sets an
// object's security descriptor without triggering user-mode inheritance
// propagation. Stable since NT 3.1, present in all Windows versions.
unsafe extern "system" {
    #[link_name = "NtSetSecurityObject"]
    fn nt_set_security_object(
        handle: HANDLE,
        security_information: u32,
        security_descriptor: *mut std::ffi::c_void,
    ) -> i32; // NTSTATUS
}

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
pub fn has_traverse_ace(path: &Path) -> Result<bool, SandboxError> {
    let wide = path_to_wide(path);
    let (dacl_ptr, _sd_guard) = read_dacl(&wide)?;
    dacl_has_app_packages_ace(dacl_ptr, TRAVERSE_MASK)
}

/// WRITE_DAC: permission to modify the object's DACL.
const WRITE_DAC: u32 = 0x0004_0000;
/// READ_CONTROL: permission to read the object's security descriptor.
const READ_CONTROL: u32 = 0x0002_0000;
/// SECURITY_DESCRIPTOR_REVISION — required by `InitializeSecurityDescriptor`.
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
/// Fallback when no existing DACL is present.
const ACL_REVISION_FALLBACK: u32 = 2;

/// Read the DACL revision from an existing DACL via `GetAclInformation`.
/// Falls back to `ACL_REVISION` (2) when the DACL is null.
fn dacl_revision(dacl: *mut ACL, path: &std::path::Path) -> Result<u32, SandboxError> {
    if dacl.is_null() {
        return Ok(ACL_REVISION_FALLBACK);
    }
    let mut rev_info = ACL_REVISION_INFORMATION { AclRevision: 0 };
    // SAFETY: Querying revision info from a valid, non-null DACL.
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut rev_info).cast(),
            std::mem::size_of::<ACL_REVISION_INFORMATION>() as u32,
            AclRevisionInformation,
        )
    };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "GetAclInformation(AclRevisionInformation) failed for {}",
            path.display(),
        )));
    }
    Ok(rev_info.AclRevision)
}

/// RAII guard for memory allocated with `std::alloc`. Cannot use `OwnedAcl`
/// (which uses `LocalFree`) because we allocate with `alloc_zeroed`.
struct AllocGuard {
    ptr: *mut u8,
    layout: std::alloc::Layout,
}
impl Drop for AllocGuard {
    fn drop(&mut self) {
        // SAFETY: ptr was allocated with std::alloc::alloc_zeroed with this layout.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

/// Grant `FILE_TRAVERSE | SYNCHRONIZE | FILE_READ_ATTRIBUTES` with
/// `NO_INHERITANCE` for ALL APPLICATION PACKAGES on a single directory.
///
/// Uses `NtSetSecurityObject` (kernel API) instead of the user-mode
/// `SetNamedSecurityInfoW` / `SetSecurityInfo`. The user-mode APIs
/// re-evaluate auto-inheritance for the entire subtree, which takes
/// minutes on directories like `C:\Users` with large subtrees. The
/// kernel API writes only the target object's security descriptor
/// without any propagation, and preserves inherited ACE flags exactly.
///
/// Reads the DACL once and checks for the ACE in the same read to avoid
/// the TOCTOU race of separate check-then-modify calls. If the ACE already
/// exists, no write is attempted.
pub fn grant_traverse(path: &Path) -> crate::Result<()> {
    let wide = path_to_wide(path);

    let (dacl_ptr, sd_guard) = read_dacl(&wide)?;

    // Allocate the SID once so it can be reused for both the check and the
    // ACE insertion, avoiding a redundant second allocation.
    let app_sid = allocate_app_packages_sid()?;

    // Check in the same DACL read whether the ACE already exists.
    if dacl_has_ace_for_sid(dacl_ptr, TRAVERSE_MASK, &app_sid)? {
        return Ok(());
    }

    // ACE not present — build new DACL by copying existing ACEs byte-for-byte
    // (preserving INHERITED_ACE flags) and appending the traverse ACE.
    //
    // Cannot use SetEntriesInAclW here for two reasons:
    // 1. SetEntriesInAclW strips INHERITED_ACE flags from existing ACEs. When
    //    paired with NtSetSecurityObject (which does NOT re-propagate inheritance),
    //    the result is a DACL where previously inherited ACEs lose their inherited
    //    flag, corrupting the inheritance state.
    // 2. SetEntriesInAclW produces an ACL meant for SetNamedSecurityInfoW, which
    //    triggers O(subtree) inheritance propagation — exactly what we avoid by
    //    using NtSetSecurityObject.

    // Read the revision from the existing DACL so the new ACL matches.
    // Avoids hardcoding ACL_REVISION (2), which would fail if the original
    // DACL uses ACL_REVISION_DS (4) on non-standard configurations.
    let dacl_revision = dacl_revision(dacl_ptr, path)?;

    // Get existing ACL size info.
    let mut acl_info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    #[allow(clippy::cast_possible_truncation)]
    let ret = unsafe {
        GetAclInformation(
            dacl_ptr,
            (&raw mut acl_info).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "GetAclInformation failed for {}",
            path.display(),
        )));
    }

    // Calculate size for the new ACL: existing bytes + new ACE.
    // ACCESS_ALLOWED_ACE struct size minus the SidStart DWORD, plus SID length.
    let sid_len = unsafe { windows_sys::Win32::Security::GetLengthSid(app_sid.as_raw()) };
    let extra_bytes = std::mem::size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
        as u32
        - std::mem::size_of::<u32>() as u32
        + sid_len;
    // Align to DWORD boundary.
    let extra_bytes = (extra_bytes + 3) & !3;
    let new_acl_size = acl_info.AclBytesInUse + extra_bytes;

    // Allocate and initialize the new ACL.
    let acl_layout = std::alloc::Layout::from_size_align(new_acl_size as usize, 4)
        .map_err(|_| SandboxError::Setup(format!("invalid ACL layout for {}", path.display())))?;
    // SAFETY: Layout is valid (size > 0, alignment is 4).
    #[allow(clippy::cast_ptr_alignment)] // ACL alignment is 2, alloc alignment is 4.
    let new_acl_ptr = unsafe { std::alloc::alloc_zeroed(acl_layout) }.cast::<ACL>();
    if new_acl_ptr.is_null() {
        return Err(SandboxError::Setup(format!(
            "ACL allocation failed for {}",
            path.display(),
        )));
    }

    // Wrap the ACL so it's freed on any early return from this point.
    let _acl_guard = AllocGuard {
        ptr: new_acl_ptr.cast(),
        layout: acl_layout,
    };

    // SAFETY: Initializing a freshly allocated, zeroed ACL buffer.
    let ret = unsafe { InitializeAcl(new_acl_ptr, new_acl_size, dacl_revision) };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "InitializeAcl failed for {}",
            path.display(),
        )));
    }

    // Copy all existing ACEs byte-for-byte, preserving INHERITED_ACE flags.
    for i in 0..acl_info.AceCount {
        let mut ace_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        // SAFETY: Reading ACE at valid index from a valid DACL.
        let ret = unsafe { windows_sys::Win32::Security::GetAce(dacl_ptr, i, &raw mut ace_ptr) };
        if ret == FALSE {
            return Err(SandboxError::Setup(format!(
                "GetAce failed for {} at index {i}",
                path.display(),
            )));
        }
        // SAFETY: ace_ptr points to a valid ACE. The ACE size is in its header.
        let ace_header = unsafe { &*(ace_ptr.cast::<windows_sys::Win32::Security::ACE_HEADER>()) };
        let ace_size = ace_header.AceSize;
        // SAFETY: Adding the ACE (preserving all flags) to the new ACL.
        let ret = unsafe {
            AddAce(
                new_acl_ptr,
                dacl_revision,
                u32::MAX, // Append at end.
                ace_ptr,
                u32::from(ace_size),
            )
        };
        if ret == FALSE {
            return Err(SandboxError::Setup(format!(
                "AddAce failed for {} at index {i}",
                path.display(),
            )));
        }
    }

    // Append the new traverse ACE (explicit, no inheritance).
    // SAFETY: Adding a well-formed ACE with a valid SID to a valid ACL.
    let ret = unsafe {
        AddAccessAllowedAceEx(
            new_acl_ptr,
            dacl_revision,
            0,
            TRAVERSE_MASK,
            app_sid.as_raw(),
        )
    };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "AddAccessAllowedAceEx failed for {}",
            path.display(),
        )));
    }

    // Build a security descriptor containing the merged DACL.
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

    // SAFETY: Attaching the manually-constructed DACL to the security
    // descriptor. The DACL memory (owned by _acl_guard) must outlive
    // the NtSetSecurityObject call.
    let ret = unsafe { SetSecurityDescriptorDacl((&raw mut sd).cast(), 1, new_acl_ptr, FALSE) };
    if ret == FALSE {
        return Err(SandboxError::Setup(format!(
            "SetSecurityDescriptorDacl failed for {}",
            path.display(),
        )));
    }

    // Preserve the original SD's DACL-related control flags (especially
    // SE_DACL_AUTO_INHERITED). Without this, SetNamedSecurityInfoW on child
    // objects treats the parent's DACL as "legacy" and may not correctly
    // re-derive inherited ACEs, causing children to lose inherited ACEs.
    let mut original_control: SECURITY_DESCRIPTOR_CONTROL = 0;
    let mut revision: u32 = 0;
    // SAFETY: Reading control flags from the original security descriptor.
    let ret = unsafe {
        GetSecurityDescriptorControl(
            sd_guard.as_raw(),
            &raw mut original_control,
            &raw mut revision,
        )
    };
    if ret == FALSE {
        let err = std::io::Error::last_os_error();
        return Err(SandboxError::Setup(format!(
            "GetSecurityDescriptorControl failed for {}: {err}",
            path.display(),
        )));
    }

    // Mask to DACL-related control bits only.
    let dacl_flags = original_control
        & (0x0400 as SECURITY_DESCRIPTOR_CONTROL  // SE_DACL_AUTO_INHERITED
            | 0x0100  // SE_DACL_AUTO_INHERIT_REQ
            | 0x0004); // SE_DACL_PROTECTED
    if dacl_flags != 0 {
        // SAFETY: Setting control bits on a valid, initialized security descriptor.
        let ret =
            unsafe { SetSecurityDescriptorControl((&raw mut sd).cast(), dacl_flags, dacl_flags) };
        // Best-effort: some security descriptors reject certain control bit
        // combinations (e.g. error 87 on system directories). The DACL is
        // structurally valid without these flags; children may just miss the
        // SE_DACL_AUTO_INHERITED hint until re-inherited.
        if ret == FALSE {
            #[cfg(debug_assertions)]
            {
                let err = std::io::Error::last_os_error();
                eprintln!(
                    "SetSecurityDescriptorControl best-effort failure for {}: {err}",
                    path.display(),
                );
            }
        }
    }

    // Open directory handle with WRITE_DAC + READ_CONTROL.
    // FILE_FLAG_BACKUP_SEMANTICS is required to open a directory handle.
    // SAFETY: Opening an existing directory for DACL modification.
    let handle: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            WRITE_DAC | READ_CONTROL,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };

    if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        let os_err = std::io::Error::last_os_error();
        #[allow(clippy::cast_possible_wrap)]
        if os_err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            return Err(SandboxError::Setup(format!(
                "cannot modify DACL for {}: {ELEVATION_REQUIRED_MARKER} (run as administrator)",
                path.display(),
            )));
        }
        return Err(SandboxError::Setup(format!(
            "failed to open {} for DACL modification: {os_err}",
            path.display(),
        )));
    }

    // SAFETY: NtSetSecurityObject is the kernel API that writes the
    // security descriptor directly without user-mode inheritance
    // propagation. Unlike SetNamedSecurityInfoW and SetSecurityInfo,
    // it does not walk the subtree to re-evaluate inherited ACEs on
    // descendants. Inherited ACE flags in the DACL are preserved exactly.
    let ntstatus =
        unsafe { nt_set_security_object(handle, DACL_SECURITY_INFORMATION, (&raw mut sd).cast()) };

    // SAFETY: Closing a valid handle obtained from CreateFileW.
    unsafe {
        CloseHandle(handle);
    }

    if ntstatus < 0 {
        // STATUS_ACCESS_DENIED = 0xC0000022
        #[allow(clippy::cast_possible_wrap)]
        if ntstatus == 0xC000_0022_u32 as i32 {
            return Err(SandboxError::Setup(format!(
                "cannot modify DACL for {}: {ELEVATION_REQUIRED_MARKER} (run as administrator)",
                path.display(),
            )));
        }
        return Err(SandboxError::Setup(format!(
            "NtSetSecurityObject failed for {} (NTSTATUS: {ntstatus:#010X})",
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
        // panic and returns a Result.
        let system_root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| r"C:\Windows".to_string());
        let _result: bool = has_traverse_ace(Path::new(&system_root)).unwrap();
    }
}
