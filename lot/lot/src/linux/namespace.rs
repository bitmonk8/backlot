#![allow(unsafe_code)]

use std::ffi::CString;
use std::fs;
use std::io;
use std::path::Path;

use crate::SandboxPolicy;

/// Convert a Path to &str, returning an error with `label` context on non-UTF-8 paths.
fn path_to_str<'a>(path: &'a Path, label: &str) -> io::Result<&'a str> {
    path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("non-UTF-8 {label} path: {}", path.display()),
        )
    })
}

/// Check whether unprivileged user namespaces are available.
pub fn is_available() -> bool {
    if is_apparmor_restricted() {
        return false;
    }

    fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone").map_or_else(
        // Sysctl absent — kernel may still support user namespaces.
        // Fall back to a runtime probe. System errors (fork/waitpid
        // failure) are treated as unavailable rather than panicking,
        // since this is a capability probe.
        |_| probe_clone_newuser().unwrap_or(false),
        |contents| contents.trim() == "1",
    )
}

/// AppArmor can block unprivileged user namespaces even when the kernel supports them.
fn is_apparmor_restricted() -> bool {
    fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .is_ok_and(|v| v.trim() == "1")
}

/// Fork a child that attempts `clone(CLONE_NEWUSER)` and immediately exits.
///
/// Returns `Ok(true)` if user namespaces are available, `Ok(false)` if the
/// kernel rejected the namespace creation, or `Err` on system errors (fork/waitpid).
///
/// The fork()+unshare()+_exit() probe uses only async-signal-safe functions
/// between fork and _exit, so it is safe even in multi-threaded contexts.
fn probe_clone_newuser() -> std::result::Result<bool, crate::SandboxError> {
    // SAFETY: We fork, then the child calls `_exit` immediately.
    // Between fork and _exit in the child we only call async-signal-safe
    // functions (unshare, _exit). The parent only calls waitpid.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(crate::SandboxError::Setup(format!(
                "fork() failed during user namespace probe: {}",
                std::io::Error::last_os_error()
            )));
        }
        if pid == 0 {
            // Child: try to create a user namespace, then exit.
            let rc = libc::unshare(libc::CLONE_NEWUSER);
            libc::_exit(i32::from(rc != 0));
        }
        // Parent: wait for child and check exit status.
        let mut status: libc::c_int = 0;
        let waited = libc::waitpid(pid, &raw mut status, 0);
        if waited < 0 {
            return Err(crate::SandboxError::Setup(format!(
                "waitpid() failed during user namespace probe: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0)
    }
}

/// Set up UID/GID mappings inside a freshly-created user namespace.
///
/// Maps container UID 0 → host `uid` and container GID 0 → host `gid`.
/// Must be called from the process that called `unshare(CLONE_NEWUSER)`.
pub fn setup_user_namespace(uid: u32, gid: u32) -> io::Result<()> {
    // Must deny setgroups before writing gid_map (kernel requirement since 3.19)
    fs::write("/proc/self/setgroups", "deny")?;
    fs::write("/proc/self/uid_map", format!("0 {uid} 1"))?;
    fs::write("/proc/self/gid_map", format!("0 {gid} 1"))?;
    Ok(())
}

/// Mount system library/binary directories and /etc config files.
fn mount_system_paths(new_root: &str, policy: &SandboxPolicy) -> io::Result<()> {
    // System library and binary directories: always mounted so the dynamic
    // linker and shared libraries are available for execve.
    for sys_path in &["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/lib32"] {
        if Path::new(sys_path).exists() {
            let dest = format!("{new_root}{sys_path}");
            mkdir_p(&dest)?;
            bind_mount_exec(sys_path, &dest)?;
        }
    }

    for bin_path in &["/bin", "/usr/bin", "/sbin", "/usr/sbin"] {
        if Path::new(bin_path).exists() {
            let dest = format!("{new_root}{bin_path}");
            mkdir_p(&dest)?;
            bind_mount_exec(bin_path, &dest)?;
        }
    }

    // Mount only specific /etc files needed by the dynamic linker and resolver.
    mkdir_p(&format!("{new_root}/etc"))?;

    for etc_file in &["/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/nsswitch.conf"] {
        if Path::new(etc_file).exists() {
            let dest = format!("{new_root}{etc_file}");
            bind_mount_file_readonly(etc_file, &dest)?;
        }
    }

    // Network-dependent config files
    if policy.allow_network() {
        if Path::new("/etc/resolv.conf").exists() {
            let dest = format!("{new_root}/etc/resolv.conf");
            bind_mount_file_readonly("/etc/resolv.conf", &dest)?;
        }
        if Path::new("/etc/ssl/certs").exists() {
            let dest = format!("{new_root}/etc/ssl/certs");
            mkdir_p(&dest)?;
            bind_mount_readonly("/etc/ssl/certs", &dest)?;
        }
    }

    Ok(())
}

/// Create a mount target at `dest` matching the type of `src`.
/// If `src` is a regular file, creates parent directories and an empty file.
/// Otherwise (directory, symlink-to-dir, etc.), creates the full directory path.
fn create_mount_target(src: &str, dest: &str) -> io::Result<()> {
    if Path::new(src).is_file() {
        // dest comes from format!("{new_root}{s}") where both parts are &str,
        // so parent() always yields valid UTF-8.
        if let Some(parent) = Path::new(dest).parent().and_then(|p| p.to_str()) {
            mkdir_p(parent)?;
        }
        create_mount_point_file(dest)
    } else {
        mkdir_p(dest)
    }
}

/// Mount policy-specified read/write/exec paths.
fn mount_policy_paths(new_root: &str, policy: &SandboxPolicy) -> io::Result<()> {
    for path in policy.read_paths() {
        let s = path_to_str(path, "read")?;
        let dest = format!("{new_root}{s}");
        create_mount_target(s, &dest)?;
        bind_mount_readonly(s, &dest)?;
    }

    for path in policy.write_paths() {
        let s = path_to_str(path, "write")?;
        let dest = format!("{new_root}{s}");
        create_mount_target(s, &dest)?;
        bind_mount_readwrite(s, &dest)?;
    }

    for path in policy.exec_paths() {
        let s = path_to_str(path, "exec")?;
        let dest = format!("{new_root}{s}");
        create_mount_target(s, &dest)?;
        bind_mount_exec(s, &dest)?;
    }

    Ok(())
}

/// Overmount deny paths with empty read-only tmpfs. Must happen after grant mounts.
fn mount_deny_paths(new_root: &str, policy: &SandboxPolicy) -> io::Result<()> {
    for path in policy.deny_paths() {
        let s = path_to_str(path, "deny")?;
        let dest = format!("{new_root}{s}");
        mkdir_p(&dest)?;
        mount_empty_tmpfs(&dest)?;
    }
    Ok(())
}

/// Mount /dev/null, /dev/zero, /dev/urandom via bind mount from host.
fn mount_dev_nodes(new_root: &str) -> io::Result<()> {
    for dev in &["/dev/null", "/dev/zero", "/dev/urandom"] {
        mount_dev_node(new_root, dev)?;
    }
    Ok(())
}

/// Set up the mount namespace: tmpfs root and bind mounts per policy.
///
/// This must be called after `unshare(CLONE_NEWUSER | CLONE_NEWNS)`.
/// Uses only paths from `policy` plus essential system directories.
///
/// Returns the new root path. The caller must pass it to
/// `mount_proc_in_new_root()` and `pivot_root()` in the inner child.
pub fn setup_mount_namespace(policy: &SandboxPolicy) -> io::Result<String> {
    // Include PID to avoid collisions between concurrent sandboxes
    // SAFETY: getpid has no preconditions
    let pid = unsafe { libc::getpid() };
    let new_root = format!("/tmp/lot-newroot-{pid}");

    mkdir_p(&new_root)?;
    mount_tmpfs(&new_root)?;

    // Private mount propagation so pivot_root works
    make_mount_private("/")?;
    make_mount_private(&new_root)?;

    // Essential directories in the new root
    mkdir_p(&format!("{new_root}/proc"))?;
    mkdir_p(&format!("{new_root}/dev"))?;
    mkdir_p(&format!("{new_root}/tmp"))?;

    mount_system_paths(&new_root, policy)?;
    mount_policy_paths(&new_root, policy)?;
    mount_deny_paths(&new_root, policy)?;
    mount_dev_nodes(&new_root)?;

    // NOTE: /proc mount and pivot_root happen in the inner child via
    // mount_proc_in_new_root() and pivot_root(). Mounting procfs requires
    // the caller to be inside the new PID namespace (only children after
    // fork() are), and must happen BEFORE pivot_root to avoid
    // mnt_already_visible() rejecting the mount.

    Ok(new_root)
}

/// Mount /proc inside the new root. Must be called from the inner child
/// (inside the PID namespace, before pivot_root).
pub fn mount_proc_in_new_root(new_root: &str) -> io::Result<()> {
    mount_proc(&format!("{new_root}/proc"))
}

/// Pivot into the new root and unmount the old root.
pub fn pivot_root(new_root: &str) -> io::Result<()> {
    execute_pivot_root(new_root)
}

/// Create a directory and all parents (like `mkdir -p`), ignoring EEXIST.
fn mkdir_p(path: &str) -> io::Result<()> {
    // Walk path components and create each level
    let mut current = String::new();
    for component in path.split('/') {
        if component.is_empty() {
            current.push('/');
            continue;
        }
        if !current.ends_with('/') {
            current.push('/');
        }
        current.push_str(component);

        let c_path = CString::new(current.as_str())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: c_path is a valid null-terminated string, mode 0o755 is standard
        let rc = unsafe { libc::mkdir(c_path.as_ptr(), 0o755) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EEXIST) {
                return Err(err);
            }
        }
    }
    Ok(())
}

/// Mount tmpfs at the given path.
fn mount_tmpfs(target: &str) -> io::Result<()> {
    let c_target = to_cstring(target)?;
    let c_fstype = to_cstring("tmpfs")?;
    let c_source = to_cstring("tmpfs")?;

    // SAFETY: all pointers are valid CStrings, flags are standard
    let rc = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Mount an empty read-only tmpfs at the given path. Used to mask deny paths
/// so the subtree appears as an inaccessible empty directory.
fn mount_empty_tmpfs(target: &str) -> io::Result<()> {
    let c_target = to_cstring(target)?;
    let c_fstype = to_cstring("tmpfs")?;
    let c_source = to_cstring("tmpfs")?;
    let c_opts = to_cstring("size=0")?;

    // SAFETY: all pointers are valid CStrings, flags make the mount read-only
    let rc = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            c_opts.as_ptr().cast(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set mount propagation to private so pivot_root works.
fn make_mount_private(path: &str) -> io::Result<()> {
    let c_path = to_cstring(path)?;
    // SAFETY: valid CString pointer, MS_REC|MS_PRIVATE is a remount flag combo
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            c_path.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Create an empty file at `path` to use as a bind-mount point.
fn create_mount_point_file(path: &str) -> io::Result<()> {
    let c_path = to_cstring(path)?;
    // SAFETY: valid CString pointer, creating regular file with 0o644
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o644) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is valid (checked above)
    unsafe { libc::close(fd) };
    Ok(())
}

/// Bind-mount a single file `src` to `dst` read-only.
/// Creates an empty file at `dst` as the mount point.
fn bind_mount_file_readonly(src: &str, dst: &str) -> io::Result<()> {
    create_mount_point_file(dst)?;
    bind_mount_readonly(src, dst)
}

/// Bind-mount `src` to `dst`, then remount with the specified flags.
/// The initial bind uses `MS_BIND | MS_REC`. The remount adds the caller's
/// `remount_flags` on top of `MS_BIND | MS_REC | MS_REMOUNT` so that flags
/// apply recursively to all submounts.
fn bind_mount(src: &str, dst: &str, remount_flags: libc::c_ulong) -> io::Result<()> {
    let c_src = to_cstring(src)?;
    let c_dst = to_cstring(dst)?;

    // SAFETY: valid CString pointers, MS_BIND|MS_REC is standard
    let rc = unsafe {
        libc::mount(
            c_src.as_ptr(),
            c_dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: valid CString pointer, remount with caller-specified flags
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            c_dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC | libc::MS_REMOUNT | remount_flags,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bind-mount `src` to `dst` read-only.
fn bind_mount_readonly(src: &str, dst: &str) -> io::Result<()> {
    bind_mount(
        src,
        dst,
        libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
    )
}

/// Bind-mount `src` to `dst` read-write (still nosuid/nodev).
fn bind_mount_readwrite(src: &str, dst: &str) -> io::Result<()> {
    bind_mount(src, dst, libc::MS_NOSUID | libc::MS_NODEV)
}

/// Bind-mount `src` to `dst` read-only but executable (no MS_NOEXEC).
fn bind_mount_exec(src: &str, dst: &str) -> io::Result<()> {
    bind_mount(src, dst, libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV)
}

/// Mount proc filesystem at the given path.
///
/// Must be called from a process inside the target PID namespace (i.e., after
/// fork() following unshare(CLONE_NEWPID)). The kernel rejects this with EPERM
/// if the caller is not a member of the PID namespace.
fn mount_proc(target: &str) -> io::Result<()> {
    let c_target = to_cstring(target)?;
    let c_fstype = to_cstring("proc")?;
    let c_source = to_cstring("proc")?;

    // SAFETY: valid CString pointers, mounting proc with restrictive flags
    let rc = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bind-mount a host device node into the new root.
fn mount_dev_node(new_root: &str, dev_path: &str) -> io::Result<()> {
    let dest = format!("{new_root}{dev_path}");

    create_mount_point_file(&dest)?;

    let c_src = to_cstring(dev_path)?;
    let c_dest = to_cstring(&dest)?;
    // SAFETY: valid CString pointers, bind mounting a device node
    let rc = unsafe {
        libc::mount(
            c_src.as_ptr(),
            c_dest.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Perform pivot_root: switch the root filesystem to `new_root`.
fn execute_pivot_root(new_root: &str) -> io::Result<()> {
    let old_root_path = format!("{new_root}/.old_root");
    mkdir_p(&old_root_path)?;

    let c_new = to_cstring(new_root)?;
    let c_old = to_cstring(&old_root_path)?;

    // SAFETY: valid CString pointers; pivot_root is not in libc crate,
    // so we use syscall(SYS_pivot_root, ...) directly.
    let rc = unsafe { libc::syscall(libc::SYS_pivot_root, c_new.as_ptr(), c_old.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    // chdir to new root
    let c_root = to_cstring("/")?;
    // SAFETY: valid CString pointer
    let rc = unsafe { libc::chdir(c_root.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    // Unmount old root with MNT_DETACH (lazy unmount)
    let c_old_mount = to_cstring("/.old_root")?;
    // SAFETY: valid CString pointer, MNT_DETACH for lazy unmount
    let rc = unsafe { libc::umount2(c_old_mount.as_ptr(), libc::MNT_DETACH) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    // Remove the now-empty old root directory
    // SAFETY: valid CString pointer, directory should be empty after unmount
    let rc = unsafe { libc::rmdir(c_old_mount.as_ptr()) };
    if rc != 0 {
        // Non-fatal: the directory may not be empty if unmount was lazy
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOTEMPTY) && err.raw_os_error() != Some(libc::EBUSY) {
            return Err(err);
        }
    }

    Ok(())
}

/// Helper to convert a `&str` to `CString`.
fn to_cstring(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn namespace_available_no_panic() {
        let _result = is_available();
    }

    #[test]
    fn test_path_to_str_rejects_non_utf8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let bad_bytes: &[u8] = &[0xff, 0xfe];
        let bad_path = Path::new(OsStr::from_bytes(bad_bytes));
        let result = path_to_str(bad_path, "test");
        assert!(result.is_err(), "non-UTF-8 path should be rejected");
    }

    #[test]
    fn test_mkdir_p_creates_nested() {
        let base = test_tmp_base("mkdir-nested");
        let nested = base.join("a/b/c");
        let nested_str = nested.to_str().expect("valid UTF-8 path");

        assert!(
            mkdir_p(nested_str).is_ok(),
            "mkdir_p should create nested dirs"
        );
        assert!(nested.is_dir(), "nested directory should exist");

        // Cleanup
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_mkdir_p_existing_dir() {
        let existing = std::env::temp_dir();
        let path_str = existing.to_str().unwrap_or("/tmp");
        // Should succeed without error on an already-existing directory
        assert!(
            mkdir_p(path_str).is_ok(),
            "mkdir_p on existing dir should not fail"
        );
    }

    #[test]
    fn test_probe_clone_newuser_returns_result() {
        // Should return Ok(bool), not panic, regardless of environment
        let result = probe_clone_newuser();
        assert!(
            result.is_ok(),
            "probe should not return Err on a normal system"
        );
    }

    /// Project-local test temp base to match CLAUDE.md conventions.
    fn test_tmp_base(name: &str) -> std::path::PathBuf {
        let ws_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let base = ws_root
            .join("test_tmp")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn test_create_mount_target_for_file() {
        let base = test_tmp_base("mount-target-file");

        let src_file = base.join("src.txt");
        std::fs::write(&src_file, "data").unwrap();
        let dest_file = base.join("dest_parent/dest.txt");

        let src_str = src_file.to_str().unwrap();
        let dest_str = dest_file.to_str().unwrap();
        create_mount_target(src_str, dest_str).unwrap();

        assert!(dest_file.exists(), "dest should be created as a file");
        assert!(dest_file.is_file(), "dest should be a regular file");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_create_mount_target_for_directory() {
        let base = test_tmp_base("mount-target-dir");

        let src_dir = base.join("src_dir");
        std::fs::create_dir_all(&src_dir).unwrap();
        let dest_dir = base.join("dest_dir/nested");

        let src_str = src_dir.to_str().unwrap();
        let dest_str = dest_dir.to_str().unwrap();
        create_mount_target(src_str, dest_str).unwrap();

        assert!(dest_dir.exists(), "dest should be created as a directory");
        assert!(dest_dir.is_dir(), "dest should be a directory");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── mkdir_p edge cases ───────────────────────────────────────────

    #[test]
    fn test_mkdir_p_absolute_root() {
        // Creating "/" should succeed (already exists).
        assert!(mkdir_p("/").is_ok());
    }

    #[test]
    fn test_mkdir_p_single_component() {
        let base = test_tmp_base("mkdir-single");
        let target = format!("{}/single", base.display());
        mkdir_p(&target).unwrap();
        assert!(Path::new(&target).is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_mkdir_p_null_byte_in_path() {
        let result = mkdir_p("/tmp/lot-test-\0-bad");
        assert!(result.is_err(), "null byte in path should fail");
    }

    #[test]
    fn test_mkdir_p_deeply_nested() {
        let base = test_tmp_base("mkdir-deep");
        let target = format!("{}/a/b/c/d/e/f", base.display());
        mkdir_p(&target).unwrap();
        assert!(Path::new(&target).is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    // ── create_mount_point_file tests ────────────────────────────────

    #[test]
    fn test_create_mount_point_file_creates_empty_file() {
        let base = test_tmp_base("mount-point-file");
        let file_path = format!("{}/mount_point", base.display());
        create_mount_point_file(&file_path).unwrap();
        assert!(Path::new(&file_path).is_file());
        // File should be empty.
        let contents = std::fs::read(&file_path).unwrap();
        assert!(contents.is_empty(), "mount point file should be empty");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_create_mount_point_file_null_byte() {
        let result = create_mount_point_file("/tmp/lot-\0-bad");
        assert!(result.is_err());
    }

    // ── create_mount_target with nonexistent src ─────────────────────

    #[test]
    fn test_create_mount_target_nonexistent_src_creates_directory() {
        // When src does not exist, Path::is_file() returns false, so mkdir_p is called.
        let base = test_tmp_base("mount-target-noexist");
        let src = format!("{}/no_such_src", base.display());
        let dest = format!("{}/dest_dir", base.display());
        create_mount_target(&src, &dest).unwrap();
        assert!(Path::new(&dest).is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }
}
