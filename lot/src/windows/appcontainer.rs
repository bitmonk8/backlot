use crate::Result;

/// Check whether `AppContainer` is available on this Windows version.
pub const fn available() -> bool {
    // TODO: check Windows version (10 1703+ for LPAC)
    true
}

/// Restore ACLs from stale sentinel files left by crashed sessions.
#[allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)]
pub fn cleanup_stale() -> Result<()> {
    // TODO: scan for sentinel files, restore original DACLs
    Ok(())
}
