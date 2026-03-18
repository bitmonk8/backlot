#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

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
/// Includes `/System/Cryptexes/OS` because macOS 13+ moves system binaries there.
const EXEC_SYSTEM_PATHS: &[&str] = &[
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/System/Cryptexes/OS/usr/bin",
    "/System/Cryptexes/OS/bin",
    "/System/Cryptexes/OS/usr/sbin",
    "/System/Cryptexes/OS/sbin",
];

/// Check whether Seatbelt (sandbox_init) is available.
pub const fn available() -> bool {
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

    // System essentials — dylibs, frameworks, and basic devices.
    // Includes cryptex paths because macOS 13+ moves system libraries there.
    // Root directory needs file-read* (not just metadata) because dyld/libSystem
    // does readdir("/") or readlink("/") during process startup.
    profile.push_str("(allow file-read* (literal \"/\"))\n");
    profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System/Library\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System/Cryptexes\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library/Preferences\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library/Apple\"))\n");
    profile.push_str("(allow file-read* (subpath \"/private/var/db/dyld\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/urandom\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/random\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/null\"))\n");

    // dyld maps shared libraries with executable permissions. file-read* does
    // NOT cover this — file-map-executable is a separate sandbox operation.
    // Without it, dyld aborts the process with SIGABRT on macOS 13+.
    profile.push_str("(allow file-map-executable\n");
    profile.push_str("  (subpath \"/usr/lib\")\n");
    profile.push_str("  (subpath \"/System/Library/Frameworks\")\n");
    profile.push_str("  (subpath \"/System/Library/PrivateFrameworks\")\n");
    profile.push_str("  (subpath \"/System/Library/Extensions\")\n");
    profile.push_str("  (subpath \"/Library/Apple/System/Library/Frameworks\")\n");
    profile.push_str("  (subpath \"/Library/Apple/System/Library/PrivateFrameworks\")\n");
    profile.push_str("  (subpath \"/Library/Apple/usr/lib\"))\n");

    // Allow writing to stdout/stderr pipes and /dev/null.
    profile.push_str("(allow file-write-data (literal \"/dev/null\"))\n");
    profile.push_str("(allow file-write-data (subpath \"/dev/fd\"))\n");

    // Scoped file-read-metadata: system paths needed for stat() resolution
    for sys_path in METADATA_SYSTEM_PATHS {
        profile.push_str("(allow file-read-metadata (literal \"");
        profile.push_str(sys_path);
        profile.push_str("\"))\n");
    }
    // Also allow metadata on all policy-granted paths
    for path in policy.read_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read-metadata", "subpath", path);
    }
    for path in policy.write_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read-metadata", "subpath", path);
    }
    for path in policy.exec_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read-metadata", "subpath", path);
    }

    // Scoped process-exec: target binary + exec_paths + system bin dirs.
    // Each also needs file-read* and file-map-executable so dyld can load the binary.
    append_sbpl_rule(
        &mut profile,
        "allow",
        "process-exec",
        "literal",
        program_path,
    );
    append_sbpl_rule(&mut profile, "allow", "file-read*", "literal", program_path);
    append_sbpl_rule(
        &mut profile,
        "allow",
        "file-map-executable",
        "literal",
        program_path,
    );
    for path in policy.exec_paths() {
        append_sbpl_rule(&mut profile, "allow", "process-exec", "subpath", path);
        append_sbpl_rule(
            &mut profile,
            "allow",
            "file-map-executable",
            "subpath",
            path,
        );
    }
    for sys_path in EXEC_SYSTEM_PATHS {
        let sys = std::path::Path::new(sys_path);
        append_sbpl_rule(&mut profile, "allow", "process-exec", "subpath", sys);
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", sys);
        append_sbpl_rule(&mut profile, "allow", "file-map-executable", "subpath", sys);
    }

    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");

    // Process needs to inspect its own info during startup (dyld, libSystem).
    profile.push_str("(allow process-info* (target self))\n");

    // Unrestricted mach-lookup is an accepted risk (see docs/DESIGN.md):
    // narrowing to specific Mach service names would break most programs
    // because the required services vary by macOS version and application.
    // This is consistent with Chrome and Firefox sandbox profiles.
    profile.push_str("(allow mach-lookup)\n");

    // Only allow sending signals to self
    profile.push_str("(allow signal (target self))\n");

    // IOKit access needed by some system libraries during startup.
    profile.push_str("(allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n");

    // Policy-specified read paths
    for path in policy.read_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", path);
    }

    // Policy-specified write paths get both read and write
    for path in policy.write_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", path);
        append_sbpl_rule(&mut profile, "allow", "file-write*", "subpath", path);
    }

    // Exec paths get read access (needed to load the binary)
    for path in policy.exec_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", path);
    }

    // Deny rules override grants above. SBPL uses last-match-wins, so these
    // must appear after the allow rules for the denied subtrees.
    for path in policy.deny_paths() {
        append_sbpl_rule(&mut profile, "deny", "file-read*", "subpath", path);
        // file-read-metadata is a separate SBPL operation not covered by file-read*
        append_sbpl_rule(&mut profile, "deny", "file-read-metadata", "subpath", path);
        append_sbpl_rule(&mut profile, "deny", "file-write*", "subpath", path);
        append_sbpl_rule(&mut profile, "deny", "process-exec", "subpath", path);
        append_sbpl_rule(&mut profile, "deny", "file-map-executable", "subpath", path);
    }

    // Ancestor directory metadata: macOS needs stat() on every component of a
    // path to traverse to it. Grant file-read-metadata on each ancestor of
    // every policy path so the kernel allows directory traversal.
    let ancestor_dirs = collect_ancestor_dirs(policy, program_path);
    for ancestor in &ancestor_dirs {
        append_sbpl_rule(
            &mut profile,
            "allow",
            "file-read-metadata",
            "literal",
            ancestor,
        );
    }

    // Network access
    if policy.allow_network() {
        profile.push_str("(allow network*)\n");
    }

    profile
}

/// Collect ancestor directories of all policy paths and the program path that
/// need file-read-metadata grants for directory traversal. Excludes `/` (already
/// granted) and the policy paths themselves (already have broader grants via
/// subpath rules).
fn collect_ancestor_dirs(policy: &SandboxPolicy, program_path: &Path) -> Vec<PathBuf> {
    let mut policy_paths: HashSet<PathBuf> = HashSet::new();
    let mut ancestors: HashSet<PathBuf> = HashSet::new();

    let all_raw_paths = policy
        .read_paths()
        .iter()
        .chain(policy.write_paths().iter())
        .chain(policy.exec_paths().iter());

    for raw in all_raw_paths {
        let resolved = resolve_path(raw);
        policy_paths.insert(resolved.clone());
        add_ancestors(&resolved, &mut ancestors);
    }

    add_ancestors(&resolve_path(program_path), &mut ancestors);

    // Remove policy paths themselves — they already have broader grants.
    for p in &policy_paths {
        ancestors.remove(p);
    }

    // Also exclude paths already covered by METADATA_SYSTEM_PATHS.
    for sys in METADATA_SYSTEM_PATHS {
        ancestors.remove(Path::new(sys));
    }

    let mut sorted: Vec<PathBuf> = ancestors.into_iter().collect();
    sorted.sort();
    sorted
}

/// Walk parent directories up to (but excluding) `/` and insert each into `set`.
fn add_ancestors(path: &Path, set: &mut HashSet<PathBuf>) {
    if !path.is_absolute() {
        return;
    }
    let mut current = path;
    while let Some(parent) = current.parent() {
        if parent == Path::new("/") {
            break;
        }
        set.insert(parent.to_path_buf());
        current = parent;
    }
}

/// Escape a path string for use in SBPL rules.
/// Replaces `"` with `\"` and rejects null bytes.
/// Returns an error if the path is not valid UTF-8 — `display()` would
/// silently replace non-UTF-8 bytes with U+FFFD, corrupting the path in
/// the generated SBPL profile.
fn escape_sbpl_path(path: &Path) -> std::result::Result<String, &'static str> {
    let s = path.to_str().ok_or("path is not valid UTF-8")?;
    if s.as_bytes().contains(&0) {
        return Err("path contains null byte");
    }
    Ok(s.replace('"', "\\\"").replace(')', "\\)"))
}

/// Resolve a path through canonicalize, falling back to the original if the
/// path doesn't exist yet. SBPL checks against resolved (real) paths, so
/// symlinks like /tmp → /private/tmp must be resolved.
fn resolve_path(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Append an SBPL `(<action> <op> (<filter> "<path>"))` rule.
/// Resolves symlinks via canonicalize because SBPL matches real paths.
fn append_sbpl_rule(
    profile: &mut String,
    action: &str,
    operation: &str,
    filter: &str,
    path: &Path,
) {
    let resolved = resolve_path(path);
    let Ok(escaped) = escape_sbpl_path(&resolved) else {
        return;
    };
    profile.push('(');
    profile.push_str(action);
    profile.push(' ');
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
    let rc = unsafe { sandbox_init(c_profile.as_ptr(), 0, &raw mut errorbuf) };

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
        SandboxPolicy::new(
            vec![PathBuf::from("/tmp/test_read")],
            vec![PathBuf::from("/tmp/test_write")],
            vec![PathBuf::from("/opt/mybin")],
            Vec::new(),
            false,
            ResourceLimits::default(),
        )
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
        assert!(profile.contains("(allow file-read* (literal \"/\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/usr/lib\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/System/Library\"))"));
        assert!(profile.contains("(allow file-read* (literal \"/dev/null\"))"));
        assert!(profile.contains("(allow file-map-executable"));
        assert!(profile.contains("(allow process-fork)"));
        assert!(profile.contains("(allow process-info* (target self))"));
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
        // Exec paths also get file-map-executable for dyld
        assert!(profile.contains("(allow file-map-executable (literal \"/usr/bin/true\"))"));
        assert!(profile.contains("(allow file-map-executable (subpath \"/opt/mybin\"))"));
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
        // Rebuild with network enabled
        policy = SandboxPolicy::new(
            policy.read_paths().to_vec(),
            policy.write_paths().to_vec(),
            policy.exec_paths().to_vec(),
            policy.deny_paths().to_vec(),
            true,
            policy.limits().clone(),
        );
        let profile = generate_profile(&policy, &test_program());
        assert!(profile.contains("(allow network*)"));
        // Redundant sub-rules should not be present
        assert!(!profile.contains("(allow network-outbound)"));
        assert!(!profile.contains("(allow network-inbound)"));
    }

    #[test]
    fn profile_empty_paths() {
        let policy = SandboxPolicy::new(
            vec![],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );
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

    #[test]
    fn ancestor_dirs_basic() {
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/a/b/c/d")],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );
        let ancestors = collect_ancestor_dirs(&policy, &test_program());
        // /a, /a/b, /a/b/c — but not "/" and not the path itself
        assert!(ancestors.contains(&PathBuf::from("/a")));
        assert!(ancestors.contains(&PathBuf::from("/a/b")));
        assert!(ancestors.contains(&PathBuf::from("/a/b/c")));
        assert!(!ancestors.contains(&PathBuf::from("/")));
        assert!(!ancestors.contains(&PathBuf::from("/a/b/c/d")));
    }

    #[test]
    fn ancestor_dirs_deduplicates() {
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/a/b/c")],
            vec![PathBuf::from("/a/b/d")],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );
        let ancestors = collect_ancestor_dirs(&policy, &test_program());
        // /a and /a/b appear once each despite shared ancestry
        assert_eq!(
            ancestors
                .iter()
                .filter(|p| *p == &PathBuf::from("/a"))
                .count(),
            1
        );
        assert_eq!(
            ancestors
                .iter()
                .filter(|p| *p == &PathBuf::from("/a/b"))
                .count(),
            1
        );
    }

    #[test]
    fn ancestor_dirs_excludes_system_metadata_paths() {
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/usr/local/share/data")],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );
        let ancestors = collect_ancestor_dirs(&policy, &test_program());
        // /usr and /usr/local are in METADATA_SYSTEM_PATHS, so excluded
        assert!(!ancestors.contains(&PathBuf::from("/usr")));
        assert!(!ancestors.contains(&PathBuf::from("/usr/local")));
        // /usr/local/share is NOT in METADATA_SYSTEM_PATHS, so included
        assert!(ancestors.contains(&PathBuf::from("/usr/local/share")));
    }

    #[test]
    fn ancestor_dirs_empty_policy() {
        let policy = SandboxPolicy::new(
            vec![],
            vec![],
            vec![],
            vec![],
            false,
            ResourceLimits::default(),
        );
        // Program path /usr/bin/true still contributes /usr/bin as ancestor
        // (/usr is excluded via METADATA_SYSTEM_PATHS).
        let ancestors = collect_ancestor_dirs(&policy, &test_program());
        assert!(ancestors.contains(&PathBuf::from("/usr/bin")));
        assert!(!ancestors.contains(&PathBuf::from("/usr")));
    }

    #[test]
    fn profile_contains_ancestor_metadata_rules() {
        // /opt/mybin from exec_paths should produce ancestor /opt
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program());
        assert!(
            profile.contains("(allow file-read-metadata (literal \"/opt\"))"),
            "profile should contain ancestor metadata for /opt"
        );
    }

    #[test]
    fn ancestor_metadata_before_network() {
        let bp = basic_policy();
        let policy = SandboxPolicy::new(
            bp.read_paths().to_vec(),
            bp.write_paths().to_vec(),
            bp.exec_paths().to_vec(),
            bp.deny_paths().to_vec(),
            true,
            bp.limits().clone(),
        );
        let profile = generate_profile(&policy, &test_program());
        let ancestor_pos = profile.find("(allow file-read-metadata (literal \"/opt\"))");
        let network_pos = profile.find("(allow network*)");
        assert!(
            ancestor_pos.is_some() && network_pos.is_some(),
            "both ancestor metadata and network rules must be present"
        );
        assert!(
            ancestor_pos.unwrap() < network_pos.unwrap(),
            "ancestor metadata rules must appear before network rules"
        );
    }
}
