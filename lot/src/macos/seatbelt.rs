#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int};
use std::path::Path;

use crate::policy::SandboxPolicy;

// SAFETY: These are Apple's sandbox API functions, always available on macOS.
unsafe extern "C" {
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

/// System paths that always get file-read-metadata access.
/// Programs need stat() on these to locate libraries and resolve paths.
const METADATA_SYSTEM_PATHS: &[&str] = &[
    "/",
    "/usr",
    "/usr/lib",
    "/usr/local",
    "/System",
    "/System/Library",
    "/Library",
    "/Library/Preferences",
    "/private",
    "/private/var",
    "/private/var/db",
    "/private/var/db/dyld",
    "/dev",
    "/tmp",
    "/var",
];

/// System paths where exec is allowed (shells, coreutils).
const EXEC_SYSTEM_PATHS: &[&str] = &["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// Check whether Seatbelt (sandbox_init) is available.
pub fn available() -> bool {
    // sandbox_init is available on all supported macOS versions.
    true
}

/// Generate an SBPL (Seatbelt Profile Language) profile string from a policy.
///
/// `program_path` is the absolute path to the binary that will be exec'd.
pub fn generate_profile(policy: &SandboxPolicy, program_path: &Path) -> String {
    let mut profile = String::with_capacity(2048);

    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n");

    // System essentials — dylibs, frameworks, and basic devices
    profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System/Library\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library/Preferences\"))\n");
    profile.push_str("(allow file-read* (subpath \"/private/var/db/dyld\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/urandom\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/random\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/null\"))\n");

    // Scoped file-read-metadata: system paths needed for stat() resolution
    for sys_path in METADATA_SYSTEM_PATHS {
        profile.push_str("(allow file-read-metadata (literal \"");
        profile.push_str(sys_path);
        profile.push_str("\"))\n");
    }
    // Also allow metadata on all policy-granted paths
    for path in &policy.read_paths {
        append_rule(&mut profile, "file-read-metadata", "subpath", path);
    }
    for path in &policy.write_paths {
        append_rule(&mut profile, "file-read-metadata", "subpath", path);
    }
    for path in &policy.exec_paths {
        append_rule(&mut profile, "file-read-metadata", "subpath", path);
    }

    // Scoped process-exec: target binary + exec_paths + system bin dirs
    append_rule(&mut profile, "process-exec", "literal", program_path);
    for path in &policy.exec_paths {
        append_rule(&mut profile, "process-exec", "subpath", path);
    }
    for sys_path in EXEC_SYSTEM_PATHS {
        profile.push_str("(allow process-exec (subpath \"");
        profile.push_str(sys_path);
        profile.push_str("\"))\n");
    }

    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");

    // Unrestricted mach-lookup is an intentional trade-off: narrowing to specific
    // Mach service names would break most programs because the required services
    // vary by macOS version and application. This is consistent with Chrome and
    // Firefox sandbox profiles, which also leave mach-lookup unrestricted.
    profile.push_str("(allow mach-lookup)\n");

    // Only allow sending signals to self
    profile.push_str("(allow signal (target self))\n");

    // Policy-specified read paths
    for path in &policy.read_paths {
        append_rule(&mut profile, "file-read*", "subpath", path);
    }

    // Policy-specified write paths get both read and write
    for path in &policy.write_paths {
        append_rule(&mut profile, "file-read*", "subpath", path);
        append_rule(&mut profile, "file-write*", "subpath", path);
    }

    // Exec paths get read access (needed to load the binary)
    for path in &policy.exec_paths {
        append_rule(&mut profile, "file-read*", "subpath", path);
    }

    // Network access
    if policy.allow_network {
        profile.push_str("(allow network*)\n");
    }

    profile
}

/// Escape a path string for use in SBPL rules.
/// Replaces `"` with `\"` and rejects null bytes.
fn escape_sbpl_path(path: &Path) -> std::result::Result<String, &'static str> {
    let s = path.display().to_string();
    if s.as_bytes().contains(&0) {
        return Err("path contains null byte");
    }
    Ok(s.replace('"', "\\\"").replace(')', "\\)"))
}

/// Append an SBPL `(allow <op> (<filter> "<path>"))` rule.
fn append_rule(profile: &mut String, operation: &str, filter: &str, path: &Path) {
    // escape_sbpl_path only fails on null bytes; display() never produces them
    let escaped = match escape_sbpl_path(path) {
        Ok(p) => p,
        Err(_) => return,
    };
    profile.push_str("(allow ");
    profile.push_str(operation);
    profile.push_str(" (");
    profile.push_str(filter);
    profile.push_str(" \"");
    profile.push_str(&escaped);
    profile.push_str("\"))\n");
}

/// Apply a Seatbelt profile to the current process. This is permanent and
/// cannot be undone.
///
/// # Safety
/// Must only be called from a forked child process before exec. The profile
/// string must be a valid SBPL profile.
pub fn apply_profile(profile: &str) -> io::Result<()> {
    let c_profile =
        CString::new(profile).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let mut errorbuf: *mut c_char = std::ptr::null_mut();

    // SAFETY: sandbox_init is the documented macOS API for applying sandbox
    // profiles. flags=0 means the first argument is the profile string itself
    // (not a named profile). errorbuf receives error details on failure.
    let rc = unsafe { sandbox_init(c_profile.as_ptr(), 0, &mut errorbuf) };

    if rc == -1 {
        let msg = if errorbuf.is_null() {
            "sandbox_init failed with unknown error".to_string()
        } else {
            // SAFETY: sandbox_init guarantees errorbuf is a valid C string on failure
            let err_str = unsafe { CStr::from_ptr(errorbuf) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: errorbuf was allocated by sandbox_init and must be freed
            unsafe { sandbox_free_error(errorbuf) };
            err_str
        };
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, msg));
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::policy::{ResourceLimits, SandboxPolicy};
    use std::path::PathBuf;

    fn basic_policy() -> SandboxPolicy {
        SandboxPolicy {
            read_paths: vec![PathBuf::from("/tmp/test_read")],
            write_paths: vec![PathBuf::from("/tmp/test_write")],
            exec_paths: vec![PathBuf::from("/opt/mybin")],
            allow_network: false,
            limits: ResourceLimits::default(),
        }
    }

    fn test_program() -> PathBuf {
        PathBuf::from("/usr/bin/true")
    }

    #[test]
    fn profile_contains_version_and_deny_default() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
    }

    #[test]
    fn profile_contains_system_essentials() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow file-read* (subpath \"/usr/lib\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/System/Library\"))"));
        assert!(profile.contains("(allow file-read* (literal \"/dev/null\"))"));
        assert!(profile.contains("(allow process-fork)"));
        assert!(profile.contains("(allow mach-lookup)"));
    }

    #[test]
    fn profile_scoped_metadata() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        // Metadata is scoped to system paths, not blanket
        assert!(!profile.contains("(allow file-read-metadata)\n"));
        assert!(profile.contains("(allow file-read-metadata (literal \"/\"))"));
        assert!(profile.contains("(allow file-read-metadata (literal \"/usr\"))"));
        assert!(profile.contains("(allow file-read-metadata (subpath \"/tmp/test_read\"))"));
    }

    #[test]
    fn profile_scoped_exec() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        // Exec is scoped, not blanket
        assert!(!profile.contains("(allow process-exec*)"));
        assert!(profile.contains("(allow process-exec (literal \"/usr/bin/true\"))"));
        assert!(profile.contains("(allow process-exec (subpath \"/opt/mybin\"))"));
        assert!(profile.contains("(allow process-exec (subpath \"/usr/bin\"))"));
        assert!(profile.contains("(allow process-exec (subpath \"/bin\"))"));
    }

    #[test]
    fn profile_signal_self_only() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow signal (target self))"));
        assert!(!profile.contains("(allow signal)\n"));
    }

    #[test]
    fn profile_contains_read_paths() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow file-read* (subpath \"/tmp/test_read\"))"));
    }

    #[test]
    fn profile_contains_write_paths_with_read() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow file-read* (subpath \"/tmp/test_write\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp/test_write\"))"));
    }

    #[test]
    fn profile_contains_exec_paths_with_read() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow file-read* (subpath \"/opt/mybin\"))"));
    }

    #[test]
    fn profile_no_network_by_default() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(!profile.contains("(allow network*)"));
    }

    #[test]
    fn profile_with_network() {
        let mut policy = basic_policy();
        policy.allow_network = true;
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow network*)"));
        // Redundant sub-rules should not be present
        assert!(!profile.contains("(allow network-outbound)"));
        assert!(!profile.contains("(allow network-inbound)"));
    }

    #[test]
    fn profile_empty_paths() {
        let policy = SandboxPolicy {
            read_paths: vec![],
            write_paths: vec![],
            exec_paths: vec![],
            allow_network: false,
            limits: ResourceLimits::default(),
        };
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
    }

    #[test]
    fn escape_sbpl_path_quotes() {
        let path = Path::new("/tmp/has\"quote");
        let escaped = escape_sbpl_path(path).unwrap();
        assert_eq!(escaped, "/tmp/has\\\"quote");
    }

    #[test]
    fn escape_sbpl_path_paren() {
        let path = Path::new("/tmp/has)paren");
        let escaped = escape_sbpl_path(path).unwrap();
        assert_eq!(escaped, "/tmp/has\\)paren");
    }
}
