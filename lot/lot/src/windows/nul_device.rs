use crate::error::SandboxError;

const NUL_DEVICE: &str = "\\\\.\\NUL";

use super::acl_helpers::{allocate_app_packages_sid, dacl_has_app_packages_ace};
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
pub fn nul_device_accessible() -> bool {
    let wide = to_wide(NUL_DEVICE);
    let Some((dacl_ptr, _sd_guard)) = super::acl_helpers::read_dacl(&wide) else {
        return false;
    };
    dacl_has_app_packages_ace(dacl_ptr, NUL_ACCESS_MASK)
}

/// Grant ALL APPLICATION PACKAGES (`S-1-15-2-1`) read/write access to `\\.\NUL`.
pub fn grant_nul_device() -> crate::Result<()> {
    if nul_device_accessible() {
        return Ok(());
    }

    let wide = to_wide(NUL_DEVICE);
    let Some(app_sid) = allocate_app_packages_sid() else {
        return Err(SandboxError::Setup(
            "failed to create ALL APPLICATION PACKAGES SID".into(),
        ));
    };

    // Device objects have no children -- no inheritance needed.
    super::acl_helpers::apply_dacl(&wide, None, NUL_ACCESS_MASK, 0, app_sid.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_device_accessible_returns_deterministic() {
        let first: bool = nul_device_accessible();
        let second: bool = nul_device_accessible();
        // System state should not change between two immediate calls.
        assert_eq!(
            first, second,
            "nul_device_accessible should be deterministic"
        );
    }
}
