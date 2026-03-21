//! AppContainer prerequisite checking and granting.
//!
//! Orchestrates both NUL device and directory traverse ACE setup.
//! The `_for_policy` variants accept a `SandboxPolicy` and belong
//! alongside the AppContainer lifecycle.

use std::path::Path;

/// One-time elevated setup. Grants all ACEs needed for AppContainer sandboxes
/// to function correctly on Windows:
///   1. NUL device read/write for ALL APPLICATION PACKAGES
///   2. Traverse ACEs on each ancestor of the provided paths, up to (and
///      including) the volume root
///
/// Idempotent -- safe to call multiple times. Requires elevation.
pub fn grant_appcontainer_prerequisites(paths: &[&Path]) -> crate::Result<()> {
    super::nul_device::grant_nul_device()?;

    let ancestors = super::traverse_acl::compute_ancestors(paths)?;
    for ancestor in &ancestors {
        super::traverse_acl::grant_traverse(ancestor)?;
    }

    Ok(())
}

/// Checks whether all ancestors of each path (up to volume root) have the
/// ALL APPLICATION PACKAGES traverse ACE, and the NUL device ACE exists.
pub fn appcontainer_prerequisites_met(paths: &[&Path]) -> bool {
    if !super::nul_device::nul_device_accessible().unwrap_or(false) {
        return false;
    }

    let Ok(ancestors) = super::traverse_acl::compute_ancestors(paths) else {
        // Cannot canonicalize paths -- prerequisites cannot be verified.
        return false;
    };
    ancestors
        .iter()
        .all(|a| super::traverse_acl::has_traverse_ace(a).unwrap_or(false))
}

/// Checks prerequisites for all paths referenced by a [`SandboxPolicy`].
///
/// Includes deny paths because `spawn_inner` computes ancestors from all
/// paths (grants + denies), so prerequisites must cover both.
pub fn appcontainer_prerequisites_met_for_policy(policy: &crate::policy::SandboxPolicy) -> bool {
    let paths = policy.all_paths();
    appcontainer_prerequisites_met(&paths)
}

/// Grants AppContainer prerequisites for all paths referenced by a [`SandboxPolicy`].
///
/// Includes deny paths because `spawn_inner` computes ancestors from all
/// paths (grants + denies), so prerequisites must cover both.
pub fn grant_appcontainer_prerequisites_for_policy(
    policy: &crate::policy::SandboxPolicy,
) -> crate::Result<()> {
    let paths = policy.all_paths();
    grant_appcontainer_prerequisites(&paths)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn appcontainer_prerequisites_met_empty_paths() {
        // With no paths, only NUL device matters -- no ancestors to check.
        let result: bool = appcontainer_prerequisites_met(&[]);
        let nul_ok = super::super::nul_device::nul_device_accessible().unwrap_or(false);
        assert_eq!(
            result, nul_ok,
            "empty paths: prerequisites_met should equal nul_device_accessible"
        );
    }

    #[test]
    fn prerequisites_met_covers_deny_paths() {
        // Verify that the _for_policy variant includes deny paths in its check.
        use crate::policy::ResourceLimits;
        use crate::policy::SandboxPolicy;

        let tmp = std::env::temp_dir();
        let deny = tmp.join("lot_test_deny_prereq");
        if !deny.exists() {
            std::fs::create_dir(&deny).expect("create deny test dir");
        }
        let policy = SandboxPolicy::new(
            vec![tmp.clone()],
            vec![],
            vec![],
            vec![deny.clone()],
            false,
            ResourceLimits::default(),
        );
        let via_policy = super::appcontainer_prerequisites_met_for_policy(&policy);
        // Cross-check: calling with the same paths directly should agree.
        let via_direct = appcontainer_prerequisites_met(&[tmp.as_path(), deny.as_path()]);
        assert_eq!(
            via_policy, via_direct,
            "_for_policy and direct check should agree"
        );
        let _ = std::fs::remove_dir(&deny);
    }
}
