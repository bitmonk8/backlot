#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

use crate::SandboxError;
use crate::policy::SandboxPolicy;

// ── FFI declarations ─────────────────────────────────────────────────

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
pub const fn is_available() -> bool {
    // sandbox_init is available on all supported macOS versions.
    true
}

// ── Profile generation ───────────────────────────────────────────────

/// Generate an SBPL (Seatbelt Profile Language) profile string from a policy.
///
/// `program_path` is the absolute path to the binary that will be exec'd.
///
/// Returns an error if any policy path cannot be encoded into a valid SBPL rule
/// (e.g., non-UTF-8 paths or paths containing null bytes). This is intentional:
/// silently dropping a rule — especially a deny rule — would weaken the sandbox.
pub fn generate_profile(
    policy: &SandboxPolicy,
    program_path: &Path,
) -> Result<String, SandboxError> {
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
        append_sbpl_rule(&mut profile, "allow", "file-read-metadata", "subpath", path)?;
    }
    for path in policy.write_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read-metadata", "subpath", path)?;
    }
    for path in policy.exec_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read-metadata", "subpath", path)?;
    }

    // Scoped process-exec: target binary + exec_paths + system bin dirs.
    // Each also needs file-read* and file-map-executable so dyld can load the binary.
    append_sbpl_rule(
        &mut profile,
        "allow",
        "process-exec",
        "literal",
        program_path,
    )?;
    append_sbpl_rule(&mut profile, "allow", "file-read*", "literal", program_path)?;
    append_sbpl_rule(
        &mut profile,
        "allow",
        "file-map-executable",
        "literal",
        program_path,
    )?;
    for path in policy.exec_paths() {
        append_sbpl_rule(&mut profile, "allow", "process-exec", "subpath", path)?;
        append_sbpl_rule(
            &mut profile,
            "allow",
            "file-map-executable",
            "subpath",
            path,
        )?;
    }
    for sys_path in EXEC_SYSTEM_PATHS {
        let sys = std::path::Path::new(sys_path);
        append_sbpl_rule(&mut profile, "allow", "process-exec", "subpath", sys)?;
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", sys)?;
        append_sbpl_rule(&mut profile, "allow", "file-map-executable", "subpath", sys)?;
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
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", path)?;
    }

    // Policy-specified write paths get both read and write
    for path in policy.write_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", path)?;
        append_sbpl_rule(&mut profile, "allow", "file-write*", "subpath", path)?;
    }

    // Exec paths get read access (needed to load the binary)
    for path in policy.exec_paths() {
        append_sbpl_rule(&mut profile, "allow", "file-read*", "subpath", path)?;
    }

    // Deny rules override grants above. SBPL uses most-specific-match-wins
    // semantics, so these must appear after the allow rules for the denied subtrees.
    for path in policy.deny_paths() {
        append_sbpl_rule(&mut profile, "deny", "file-read*", "subpath", path)?;
        // file-read-metadata is a separate SBPL operation not covered by file-read*
        append_sbpl_rule(&mut profile, "deny", "file-read-metadata", "subpath", path)?;
        append_sbpl_rule(&mut profile, "deny", "file-write*", "subpath", path)?;
        append_sbpl_rule(&mut profile, "deny", "process-exec", "subpath", path)?;
        append_sbpl_rule(&mut profile, "deny", "file-map-executable", "subpath", path)?;
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
        )?;
    }

    // Network access
    if policy.allow_network() {
        profile.push_str("(allow network*)\n");
    }

    Ok(profile)
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
/// SBPL uses C-style escaping inside double-quoted strings: `\` → `\\`, `"` → `\"`.
/// Rejects null bytes and non-UTF-8 paths.
fn escape_sbpl_path(path: &Path) -> std::result::Result<String, &'static str> {
    let s = path.to_str().ok_or("path is not valid UTF-8")?;
    if s.as_bytes().contains(&0) {
        return Err("path contains null byte");
    }
    // Escape backslash first (so we don't double-escape the quote escape),
    // then escape double quotes.
    Ok(s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Resolve a path through canonicalize, falling back to the original if the
/// path doesn't exist yet. SBPL checks against resolved (real) paths, so
/// symlinks like /tmp → /private/tmp must be resolved.
fn resolve_path(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Append an SBPL `(<action> <op> (<filter> "<path>"))` rule.
/// Resolves symlinks via canonicalize because SBPL matches real paths.
/// Returns an error if the resolved path cannot be encoded into SBPL
/// (non-UTF-8 or contains null bytes).
fn append_sbpl_rule(
    profile: &mut String,
    action: &str,
    operation: &str,
    filter: &str,
    path: &Path,
) -> Result<(), SandboxError> {
    let resolved = resolve_path(path);
    let escaped = escape_sbpl_path(&resolved).map_err(|reason| {
        SandboxError::Setup(format!(
            "cannot encode path into SBPL rule: {}: {}",
            path.display(),
            reason,
        ))
    })?;
    profile.push('(');
    profile.push_str(action);
    profile.push(' ');
    profile.push_str(operation);
    profile.push_str(" (");
    profile.push_str(filter);
    profile.push_str(" \"");
    profile.push_str(&escaped);
    profile.push_str("\"))\n");
    Ok(())
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
    use crate::policy::SandboxPolicy;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    fn bad_utf8_path() -> PathBuf {
        PathBuf::from(OsStr::from_bytes(b"/tmp/\xff"))
    }

    fn basic_policy() -> SandboxPolicy {
        SandboxPolicy::new(
            vec![PathBuf::from("/tmp/test_read")],
            vec![PathBuf::from("/tmp/test_write")],
            vec![PathBuf::from("/opt/mybin")],
            Vec::new(),
            false,
        )
    }

    fn test_program() -> PathBuf {
        PathBuf::from("/usr/bin/true")
    }

    fn policy_with_deny_path() -> SandboxPolicy {
        SandboxPolicy::new(
            vec![PathBuf::from("/tmp/test_read")],
            vec![PathBuf::from("/tmp/test_write")],
            vec![PathBuf::from("/opt/mybin")],
            vec![PathBuf::from("/tmp/test_read/secret")],
            false,
        )
    }

    #[test]
    fn profile_contains_version_and_deny_default() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
    }

    #[test]
    fn profile_contains_system_essentials() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
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
        let profile = generate_profile(&policy, &test_program()).unwrap();
        // Metadata is scoped to system paths, not blanket
        assert!(!profile.contains("(allow file-read-metadata)\n"));
        assert!(profile.contains("(allow file-read-metadata (literal \"/\"))"));
        assert!(profile.contains("(allow file-read-metadata (literal \"/usr\"))"));
        assert!(profile.contains("(allow file-read-metadata (subpath \"/tmp/test_read\"))"));
    }

    #[test]
    fn profile_scoped_exec() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
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
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(profile.contains("(allow signal (target self))"));
        assert!(!profile.contains("(allow signal)\n"));
    }

    #[test]
    fn profile_contains_read_paths() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(profile.contains("(allow file-read* (subpath \"/tmp/test_read\"))"));
    }

    #[test]
    fn profile_contains_write_paths_with_read() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(profile.contains("(allow file-read* (subpath \"/tmp/test_write\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp/test_write\"))"));
    }

    #[test]
    fn profile_contains_exec_paths_with_read() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(profile.contains("(allow file-read* (subpath \"/opt/mybin\"))"));
    }

    #[test]
    fn profile_no_network_by_default() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
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
        );
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(profile.contains("(allow network*)"));
        // Redundant sub-rules should not be present
        assert!(!profile.contains("(allow network-outbound)"));
        assert!(!profile.contains("(allow network-inbound)"));
    }

    #[test]
    fn profile_empty_paths() {
        let policy = SandboxPolicy::new(vec![], vec![], vec![], vec![], false);
        let profile = generate_profile(&policy, &test_program()).unwrap();
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
    fn escape_sbpl_path_paren_not_escaped() {
        let path = Path::new("/tmp/has)paren");
        let escaped = escape_sbpl_path(path).unwrap();
        // Parentheses do not need escaping inside double-quoted SBPL strings.
        assert_eq!(escaped, "/tmp/has)paren");
    }

    #[test]
    fn escape_sbpl_path_backslash() {
        let path = Path::new("/tmp/has\\backslash");
        let escaped = escape_sbpl_path(path).unwrap();
        assert_eq!(escaped, "/tmp/has\\\\backslash");
    }

    #[test]
    fn escape_sbpl_path_backslash_and_quote() {
        let path = Path::new("/tmp/a\\\"b");
        let escaped = escape_sbpl_path(path).unwrap();
        assert_eq!(escaped, "/tmp/a\\\\\\\"b");
    }

    #[test]
    fn ancestor_dirs_basic() {
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/a/b/c/d")],
            vec![],
            vec![],
            vec![],
            false,
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
        let policy = SandboxPolicy::new(vec![], vec![], vec![], vec![], false);
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
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(
            profile.contains("(allow file-read-metadata (literal \"/opt\"))"),
            "profile should contain ancestor metadata for /opt"
        );
    }

    #[test]
    fn profile_contains_deny_rules() {
        let policy = policy_with_deny_path();
        let profile = generate_profile(&policy, &test_program()).unwrap();
        for op in &[
            "file-read*",
            "file-read-metadata",
            "file-write*",
            "process-exec",
            "file-map-executable",
        ] {
            let rule = format!("(deny {op} (subpath \"/tmp/test_read/secret\"))");
            assert!(profile.contains(&rule), "profile must contain: {rule}");
        }
    }

    #[test]
    fn profile_deny_rules_after_allow_rules() {
        // SBPL uses last-match-wins, so deny rules must appear after allow rules
        // for the parent path to actually override them.
        let policy = policy_with_deny_path();
        let profile = generate_profile(&policy, &test_program()).unwrap();

        for op in &["file-read*", "file-read-metadata"] {
            let allow_rule = format!("(allow {op} (subpath \"/tmp/test_read\"))");
            let deny_rule = format!("(deny {op} (subpath \"/tmp/test_read/secret\"))");
            let allow_pos = profile
                .find(&allow_rule)
                .unwrap_or_else(|| panic!("allow {op} rule for parent must exist"));
            let deny_pos = profile
                .find(&deny_rule)
                .unwrap_or_else(|| panic!("deny {op} rule for child must exist"));
            assert!(
                deny_pos > allow_pos,
                "deny {op} must appear after allow {op} for parent"
            );
        }
    }

    #[test]
    fn profile_no_deny_rules_when_deny_paths_empty() {
        let policy = basic_policy();
        let profile = generate_profile(&policy, &test_program()).unwrap();
        assert!(
            !profile.contains("(deny file-read* (subpath"),
            "no deny file-read* rules without deny paths"
        );
        assert!(
            !profile.contains("(deny file-read-metadata (subpath"),
            "no deny file-read-metadata rules without deny paths"
        );
        assert!(
            !profile.contains("(deny file-write*"),
            "no deny file-write* rules without deny paths"
        );
        assert!(
            !profile.contains("(deny process-exec (subpath"),
            "no deny process-exec rules without deny paths"
        );
        assert!(
            !profile.contains("(deny file-map-executable (subpath"),
            "no deny file-map-executable rules without deny paths"
        );
    }

    #[test]
    fn profile_multiple_deny_paths() {
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/tmp/test_read")],
            vec![PathBuf::from("/tmp/test_write")],
            vec![PathBuf::from("/opt/mybin")],
            vec![
                PathBuf::from("/tmp/test_read/secret"),
                PathBuf::from("/tmp/test_write/private"),
            ],
            false,
        );
        let profile = generate_profile(&policy, &test_program()).unwrap();
        for op in &[
            "file-read*",
            "file-read-metadata",
            "file-write*",
            "process-exec",
            "file-map-executable",
        ] {
            for deny in &["/tmp/test_read/secret", "/tmp/test_write/private"] {
                let rule = format!("(deny {op} (subpath \"{deny}\"))");
                assert!(profile.contains(&rule), "profile must contain: {rule}");
            }
        }
    }

    #[test]
    fn profile_deny_write_path_ordering() {
        // Verify deny rules under a write-path parent also appear after allow rules.
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/tmp/test_read")],
            vec![PathBuf::from("/tmp/test_write")],
            vec![PathBuf::from("/opt/mybin")],
            vec![PathBuf::from("/tmp/test_write/private")],
            false,
        );
        let profile = generate_profile(&policy, &test_program()).unwrap();

        for op in &["file-read*", "file-write*"] {
            let allow_rule = format!("(allow {op} (subpath \"/tmp/test_write\"))");
            let deny_rule = format!("(deny {op} (subpath \"/tmp/test_write/private\"))");
            let allow_pos = profile
                .find(&allow_rule)
                .unwrap_or_else(|| panic!("allow {op} rule for parent must exist"));
            let deny_pos = profile
                .find(&deny_rule)
                .unwrap_or_else(|| panic!("deny {op} rule for child must exist"));
            assert!(
                deny_pos > allow_pos,
                "deny {op} must appear after allow {op} for parent"
            );
        }
    }

    fn assert_non_utf8_path_rejected(
        read: Vec<PathBuf>,
        write: Vec<PathBuf>,
        exec: Vec<PathBuf>,
        deny: Vec<PathBuf>,
    ) {
        let policy = SandboxPolicy::new(read, write, exec, deny, false);
        let err = generate_profile(&policy, &test_program()).unwrap_err();
        assert!(
            matches!(err, SandboxError::Setup(_)),
            "expected SandboxError::Setup, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("not valid UTF-8"),
            "expected UTF-8 error, got: {msg}"
        );
        assert!(
            msg.contains("\u{FFFD}"),
            "error should contain the replacement character from the non-UTF-8 path, got: {msg}"
        );
    }

    #[test]
    fn generate_profile_errors_on_non_utf8_read_path() {
        assert_non_utf8_path_rejected(vec![bad_utf8_path()], vec![], vec![], vec![]);
    }

    #[test]
    fn generate_profile_errors_on_non_utf8_write_path() {
        assert_non_utf8_path_rejected(vec![], vec![bad_utf8_path()], vec![], vec![]);
    }

    #[test]
    fn generate_profile_errors_on_non_utf8_exec_path() {
        assert_non_utf8_path_rejected(vec![], vec![], vec![bad_utf8_path()], vec![]);
    }

    #[test]
    fn generate_profile_errors_on_non_utf8_deny_path() {
        assert_non_utf8_path_rejected(
            vec![PathBuf::from("/tmp/test_read")],
            vec![],
            vec![],
            vec![bad_utf8_path()],
        );
    }

    #[test]
    fn generate_profile_errors_on_non_utf8_program_path() {
        let policy = SandboxPolicy::new(vec![], vec![], vec![], vec![], false);
        let err = generate_profile(&policy, &bad_utf8_path()).unwrap_err();
        assert!(
            matches!(err, SandboxError::Setup(_)),
            "expected SandboxError::Setup, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("not valid UTF-8"),
            "expected UTF-8 error, got: {msg}"
        );
        assert!(
            msg.contains("\u{FFFD}"),
            "error should contain the replacement character from the non-UTF-8 path, got: {msg}"
        );
    }

    #[test]
    fn generate_profile_errors_on_null_byte_path() {
        let null_path = PathBuf::from("/tmp/has\0null");
        let policy = SandboxPolicy::new(vec![null_path], vec![], vec![], vec![], false);
        let err = generate_profile(&policy, &test_program()).unwrap_err();
        assert!(
            matches!(err, SandboxError::Setup(_)),
            "expected SandboxError::Setup, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("null byte"),
            "expected null byte error, got: {msg}"
        );
    }

    // ── resolve_path tests ─────────────────────────────────────────

    #[test]
    fn resolve_path_follows_symlink() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let target = dir.path().join("real_file");
        std::fs::write(&target, "data").expect("write target");
        let link = dir.path().join("sym_link");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let resolved = resolve_path(&link);
        let canonical_target = std::fs::canonicalize(&target).expect("canonicalize target");
        assert_eq!(resolved, canonical_target);
    }

    #[test]
    fn resolve_path_nonexistent_returns_input() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let resolved = resolve_path(&path);
        assert_eq!(resolved, path);
    }

    // ── add_ancestors guard behavior ────────────────────────────────

    // ── apply_profile tests ─────────────────────────────────────────

    #[test]
    fn apply_profile_rejects_null_byte() {
        let result = apply_profile("(version 1)\0(deny default)");
        assert!(result.is_err(), "null byte in profile should fail");
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn apply_profile_rejects_invalid_sbpl_in_fork() {
        // apply_profile is permanent, so test in a forked child.
        // Prepare CString before fork to avoid heap allocation in the child
        // (Rust test harness is multi-threaded; allocating after fork risks
        // deadlock if another thread holds the heap lock).
        let c_profile = CString::new("not valid sbpl at all").unwrap();

        let mut fds = [0i32; 2];
        // SAFETY: fds is a valid 2-element array
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (read_fd, write_fd) = (fds[0], fds[1]);

        // SAFETY: fork is safe; child uses only async-signal-safe ops
        // plus sandbox_init (kernel FFI, does not use Rust allocator).
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(read_fd) };

            let mut errorbuf: *mut c_char = std::ptr::null_mut();
            // SAFETY: sandbox_init is async-signal-safe (kernel transition).
            // c_profile was allocated before fork and is valid.
            let rc = unsafe { sandbox_init(c_profile.as_ptr(), 0, &raw mut errorbuf) };
            let msg: &[u8] = if rc == -1 {
                if !errorbuf.is_null() {
                    // SAFETY: errorbuf allocated by sandbox_init, must be freed
                    unsafe { sandbox_free_error(errorbuf) };
                }
                b"ERR"
            } else {
                b"OK"
            };
            // SAFETY: write_fd is valid, msg is a valid buffer
            unsafe { libc::write(write_fd, msg.as_ptr().cast(), msg.len()) };
            unsafe { libc::close(write_fd) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fd
        unsafe { libc::close(write_fd) };

        let mut buf = [0u8; 16];
        // SAFETY: read_fd is valid, buf is a valid buffer
        let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
        unsafe { libc::close(read_fd) };

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = if n > 0 {
            std::str::from_utf8(&buf[..n as usize]).unwrap_or("UTF8_ERR")
        } else {
            "READ_FAIL"
        };
        assert_eq!(result, "ERR", "invalid SBPL should fail: {result}");
    }

    #[test]
    fn add_ancestors_skips_relative_path() {
        let mut set = HashSet::new();
        add_ancestors(Path::new("relative/path"), &mut set);
        assert!(set.is_empty(), "relative paths should be skipped");
    }

    #[test]
    fn add_ancestors_stops_at_root() {
        let mut set = HashSet::new();
        add_ancestors(Path::new("/a/b/c"), &mut set);
        assert!(set.contains(&PathBuf::from("/a")));
        assert!(set.contains(&PathBuf::from("/a/b")));
        assert!(!set.contains(&PathBuf::from("/")));
        assert!(!set.contains(&PathBuf::from("/a/b/c")));
    }

    #[test]
    fn add_ancestors_single_component() {
        let mut set = HashSet::new();
        add_ancestors(Path::new("/a"), &mut set);
        // /a has only / as parent, which is excluded
        assert!(set.is_empty());
    }

    // ── deny-path ancestor metadata rules ─────────────────────────

    #[test]
    fn deny_path_ancestors_excluded_from_ancestor_dirs() {
        // Deny paths should NOT contribute to ancestor metadata rules
        // because collect_ancestor_dirs only iterates read/write/exec paths.
        let policy = SandboxPolicy::new(
            vec![PathBuf::from("/tmp/test_read")],
            vec![],
            vec![],
            vec![PathBuf::from("/tmp/test_read/secret/deep")],
            false,
        );
        let ancestors = collect_ancestor_dirs(&policy, &test_program());
        // /tmp/test_read/secret should NOT appear since deny paths don't
        // contribute to ancestors (they are excluded from all_raw_paths).
        assert!(
            !ancestors.contains(&PathBuf::from("/tmp/test_read/secret")),
            "deny path ancestors should not be in ancestor_dirs"
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
        );
        let profile = generate_profile(&policy, &test_program()).unwrap();
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
