use crate::error::SandboxError;

const NUL_DEVICE: &str = "\\\\.\\NUL";

use super::acl_helpers::{
    allocate_app_packages_sid, dacl_has_ace_for_sid, dacl_has_app_packages_ace, read_dacl,
};
use super::{FILE_GENERIC_READ, FILE_GENERIC_WRITE};

use super::to_wide;

/// Access mask used for NUL device ACE checks.
const NUL_ACCESS_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE;

// ---------------------------------------------------------------------------
// NUL device functions
// ---------------------------------------------------------------------------

/// Test whether AppContainer processes can access `\\.\NUL`.
///
/// Returns `true` if either:
/// - The DACL is NULL (unrestricted access -- everyone can access the device), or
/// - The DACL contains an allow ACE for ALL APPLICATION PACKAGES (`S-1-15-2-1`)
///   with at least `FILE_GENERIC_READ | FILE_GENERIC_WRITE`.
pub fn nul_device_accessible() -> Result<bool, SandboxError> {
    let wide = to_wide(NUL_DEVICE);
    let (dacl_ptr, _sd_guard) = read_dacl(&wide)?;
    dacl_has_app_packages_ace(dacl_ptr, NUL_ACCESS_MASK)
}

/// Grant ALL APPLICATION PACKAGES (`S-1-15-2-1`) read/write access to `\\.\NUL`.
pub fn grant_nul_device() -> crate::Result<()> {
    let wide = to_wide(NUL_DEVICE);
    let (dacl_ptr, _sd_guard) = read_dacl(&wide)?;

    // Allocate the SID once so it can be reused for both the check and the
    // ACE insertion, avoiding a redundant second allocation.
    let app_sid = allocate_app_packages_sid()?;

    if dacl_has_ace_for_sid(dacl_ptr, NUL_ACCESS_MASK, &app_sid)? {
        return Ok(());
    }

    // Device objects have no children -- no inheritance needed.
    super::acl_helpers::apply_dacl(&wide, None, NUL_ACCESS_MASK, 0, app_sid.as_raw())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn nul_device_path_constant_is_correct() {
        assert_eq!(NUL_DEVICE, "\\\\.\\NUL");
    }

    #[test]
    fn nul_access_mask_includes_read_and_write() {
        assert_ne!(NUL_ACCESS_MASK & FILE_GENERIC_READ, 0);
        assert_ne!(NUL_ACCESS_MASK & FILE_GENERIC_WRITE, 0);
    }

    #[test]
    fn check_nul_device_access_does_not_error() {
        // nul_device_accessible reads the DACL on \\.\NUL.
        // It should succeed regardless of whether ACEs are in place.
        let result = nul_device_accessible();
        assert!(
            result.is_ok(),
            "reading NUL device DACL should not fail: {result:?}"
        );
    }

    #[test]
    fn nul_device_accessible_returns_deterministic() {
        // .unwrap() verifies Ok (no error). We cannot assert the bool value
        // because it depends on whether `lot setup` has been run on this machine.
        let first = nul_device_accessible().unwrap();
        let second = nul_device_accessible().unwrap();
        // System state should not change between two immediate calls.
        assert_eq!(
            first, second,
            "nul_device_accessible should be deterministic"
        );
    }
}
