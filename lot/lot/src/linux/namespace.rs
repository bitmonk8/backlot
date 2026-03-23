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
///
/// Paths that are symlinks on the host (e.g., `/lib` -> `usr/lib` on Fedora/Arch)
/// are recreated as symlinks rather than bind-mounted as directories.
fn mount_system_paths(new_root: &str, policy: &SandboxPolicy) -> io::Result<()> {
    let system_paths: &[&str] = &[
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib32",
        "/bin",
        "/usr/bin",
        "/sbin",
        "/usr/sbin",
    ];

    // Two-pass: mount real directories first, then create symlinks. Symlinks
    // may point into mounted directories (e.g., /lib -> usr/lib), so their
    // targets must exist before the symlink is created.
    let mut real_dirs = Vec::new();
    let mut symlinks = Vec::new();

    for &path in system_paths {
        let p = Path::new(path);
        // is_symlink() works even if the target doesn't exist; exists() follows symlinks
        if !p.exists() && !p.is_symlink() {
            continue;
        }
        if p.is_symlink() {
            let target = fs::read_link(p)?;
            symlinks.push((path, target));
        } else {
            real_dirs.push(path);
        }
    }

    // Mount real directories first
    for &path in &real_dirs {
        let dest = format!("{new_root}{path}");
        mkdir_p(&dest)?;
        bind_mount_exec(path, &dest)?;
    }

    // Recreate symlinks for paths that are symlinks on the host
    for (path, target) in &symlinks {
        let dest = format!("{new_root}{path}");
        if let Some(parent) = Path::new(&dest).parent() {
            mkdir_p(path_to_str(parent, "symlink parent")?)?;
        }
        std::os::unix::fs::symlink(target, &dest).or_else(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        })?;
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

    // Best-effort cleanup of stale mount point left by a crashed process
    // whose PID was recycled. Errors are ignored — if cleanup fails, the
    // subsequent mkdir_p/mount_tmpfs will produce a clear error.
    if let Ok(c_new_root) = CString::new(new_root.as_bytes()) {
        // SAFETY: c_new_root is a valid NUL-terminated path; umount2 and rmdir
        // accept any path and are async-signal-safe.
        unsafe {
            libc::umount2(c_new_root.as_ptr(), libc::MNT_DETACH);
            libc::rmdir(c_new_root.as_ptr());
        }
    }

    mkdir_p(&new_root)?;
    mount_tmpfs(&new_root)?;

    // Private mount propagation so pivot_root works
    make_mount_private("/")?;
    make_mount_private(&new_root)?;

    // Essential directories in the new root
    mkdir_p(&format!("{new_root}/proc"))?;
    mkdir_p(&format!("{new_root}/dev"))?;
    mkdir_p(&format!("{new_root}/tmp"))?;

    // Policy paths first, then system paths. System paths (libraries,
    // binaries) are mounted with exec flags and must overlay any policy
    // read_path mounts that set MS_NOEXEC on the same subtree.
    mount_policy_paths(&new_root, policy)?;
    mount_system_paths(&new_root, policy)?;
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
///
/// The initial bind uses `MS_BIND | MS_REC`. The kernel ignores `MS_REC` on
/// remount operations, so we enumerate all submounts under `dst` via
/// `/proc/self/mountinfo` and remount each individually.
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

    // Remount each submount individually (MS_REC is ignored on remounts).
    // In a user namespace, mount flags are locked — you cannot relax flags
    // inherited from the source (e.g., remove MS_NOEXEC from a snap mount).
    // EINVAL on a submount remount means the requested flags would relax a
    // locked flag. The submount retains its source flags, which are at least
    // as restrictive, so skipping is safe.
    let mounts = submounts_under(dst)?;
    for mount_point in &mounts {
        let c_mp = to_cstring(mount_point)?;
        // SAFETY: valid CString pointer, remounting individual submount
        let rc = unsafe {
            libc::mount(
                std::ptr::null(),
                c_mp.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | remount_flags,
                std::ptr::null(),
            )
        };
        if rc != 0 {
            let err = io::Error::last_os_error();
            // EINVAL on a submount (not the top-level mount) means locked
            // flags prevent the remount. The submount keeps its existing
            // (more restrictive) flags — skip it.
            if err.raw_os_error() == Some(libc::EINVAL) && *mount_point != *dst {
                continue;
            }
            return Err(err);
        }
    }
    Ok(())
}

/// Decode octal escape sequences in mountinfo paths (`\040` for space, etc.).
fn decode_mountinfo_path(encoded: &str) -> String {
    let mut result = String::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let d0 = bytes[i + 1].wrapping_sub(b'0');
            let d1 = bytes[i + 2].wrapping_sub(b'0');
            let d2 = bytes[i + 3].wrapping_sub(b'0');
            if d0 < 8 && d1 < 8 && d2 < 8 {
                let ch = u32::from(d0) * 64 + u32::from(d1) * 8 + u32::from(d2);
                // Octal escapes in mountinfo are single bytes
                result.push(ch as u8 as char);
                i += 4;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Parse mountinfo content and return all mount points at or under `prefix`,
/// sorted by path length (shortest first).
fn parse_submounts(mountinfo: &str, prefix: &str) -> Vec<String> {
    let prefix_bytes = prefix.as_bytes();

    let mut mounts = Vec::new();
    for line in mountinfo.lines() {
        // mountinfo fields: id parent_id major:minor root mount_point [options...] - fs_type source super_options
        // Field 4 (0-indexed) is the mount point.
        let Some(raw_mount) = line.split(' ').nth(4) else {
            continue;
        };
        let mount_point = decode_mountinfo_path(raw_mount);

        // Include exact match or any child path. For root prefix "/",
        // every absolute path is a child. Otherwise require a '/' separator
        // after the prefix to avoid false matches (e.g., "/pro" vs "/proc").
        let is_match = mount_point == prefix
            || (prefix == "/" && mount_point.starts_with('/'))
            || (mount_point.len() > prefix_bytes.len()
                && mount_point.as_bytes().starts_with(prefix_bytes)
                && mount_point.as_bytes()[prefix_bytes.len()] == b'/');
        if is_match {
            mounts.push(mount_point);
        }
    }

    // Sort by path length (shortest first) so parents are remounted before children
    mounts.sort_by_key(String::len);
    mounts
}

/// Read `/proc/self/mountinfo` and return all mount points at or under `prefix`,
/// sorted by path length (shortest first).
fn submounts_under(prefix: &str) -> io::Result<Vec<String>> {
    let content = fs::read_to_string("/proc/self/mountinfo")?;
    Ok(parse_submounts(&content, prefix))
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

    // ── mountinfo helper tests ──────────────────────────────────────

    #[test]
    fn test_decode_mountinfo_path_plain() {
        assert_eq!(decode_mountinfo_path("/mnt/data"), "/mnt/data");
    }

    #[test]
    fn test_decode_mountinfo_path_space() {
        // \040 is octal for space (0x20 = 32)
        assert_eq!(decode_mountinfo_path("/mnt/my\\040dir"), "/mnt/my dir");
    }

    #[test]
    fn test_decode_mountinfo_path_tab() {
        // \011 is octal for tab (0x09 = 9)
        assert_eq!(decode_mountinfo_path("/mnt/a\\011b"), "/mnt/a\tb");
    }

    #[test]
    fn test_decode_mountinfo_path_backslash() {
        // \134 is octal for backslash (0x5C = 92)
        assert_eq!(decode_mountinfo_path("/mnt/a\\134b"), "/mnt/a\\b");
    }

    #[test]
    fn test_decode_mountinfo_path_newline() {
        // \012 is octal for newline (0x0A = 10)
        assert_eq!(decode_mountinfo_path("/mnt/a\\012b"), "/mnt/a\nb");
    }

    #[test]
    fn test_decode_mountinfo_path_multiple_escapes() {
        assert_eq!(decode_mountinfo_path("/mnt/a\\040b\\040c"), "/mnt/a b c");
    }

    #[test]
    fn test_decode_mountinfo_path_trailing_backslash() {
        // Incomplete escape at end should be passed through
        assert_eq!(decode_mountinfo_path("/mnt/trail\\"), "/mnt/trail\\");
    }

    #[test]
    fn test_decode_mountinfo_path_non_octal_after_backslash() {
        // \xyz is not valid octal — pass through as-is
        assert_eq!(decode_mountinfo_path("/mnt/a\\xyz"), "/mnt/a\\xyz");
    }

    #[test]
    fn test_decode_mountinfo_path_partial_octal_two_digits() {
        // Backslash + only 2 octal digits — not enough for a complete escape
        assert_eq!(decode_mountinfo_path("/mnt/a\\04"), "/mnt/a\\04");
    }

    #[test]
    fn test_decode_mountinfo_path_partial_octal_one_digit() {
        assert_eq!(decode_mountinfo_path("/mnt/a\\0"), "/mnt/a\\0");
    }

    #[test]
    fn test_decode_mountinfo_path_escape_at_exact_end() {
        // \040 occupying the last 4 bytes — boundary for i + 3 < len check
        assert_eq!(decode_mountinfo_path("/a\\040"), "/a ");
    }

    #[test]
    fn test_decode_mountinfo_path_high_octal() {
        // \377 = 255 (0xFF), highest single-byte value
        assert_eq!(decode_mountinfo_path("/mnt/a\\377b"), "/mnt/a\u{ff}b");
    }

    #[test]
    fn test_decode_mountinfo_path_null_octal() {
        // \000 = NUL byte
        assert_eq!(decode_mountinfo_path("/mnt/a\\000b"), "/mnt/a\0b");
    }

    #[test]
    fn test_submounts_under_parses_mountinfo() {
        // submounts_under reads /proc/self/mountinfo which exists on Linux.
        // We can at least verify it returns results containing "/" for the
        // root mount point.
        let result = submounts_under("/");
        assert!(result.is_ok(), "reading mountinfo should succeed");
        let mounts = result.unwrap();
        assert!(!mounts.is_empty(), "root should have at least one mount");
        assert_eq!(mounts[0], "/", "first entry should be / (shortest)");
    }

    #[test]
    fn test_submounts_under_nonexistent_prefix() {
        let result = submounts_under("/nonexistent_prefix_abc123");
        assert!(result.is_ok());
        let mounts = result.unwrap();
        assert!(mounts.is_empty(), "no mounts should match bogus prefix");
    }

    #[test]
    fn test_parse_submounts_prefix_matching() {
        let mountinfo = "\
36 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
37 36 0:6 / /proc rw,nosuid,nodev,noexec shared:5 - proc proc rw
38 36 0:7 / /sys rw,nosuid,nodev,noexec shared:6 - sysfs sysfs rw
39 36 8:1 /mnt/data /mnt/data rw,relatime shared:1 - ext4 /dev/sda1 rw
40 36 0:40 / /pro rw - tmpfs tmpfs rw";

        // "/proc" should match exactly, not "/pro"
        let result = parse_submounts(mountinfo, "/proc");
        assert_eq!(result, vec!["/proc"]);

        // "/" should match everything
        let result = parse_submounts(mountinfo, "/");
        assert!(result.len() == 5);
        assert_eq!(result[0], "/");

        // "/mnt/data" exact match
        let result = parse_submounts(mountinfo, "/mnt/data");
        assert_eq!(result, vec!["/mnt/data"]);

        // "/nonexistent" matches nothing
        let result = parse_submounts(mountinfo, "/nonexistent");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_submounts_with_children() {
        let mountinfo = "\
36 1 8:1 / /mnt/root rw - ext4 /dev/sda1 rw
37 36 8:2 / /mnt/root/sub1 rw - ext4 /dev/sda2 rw
38 36 8:3 / /mnt/root/sub1/deep rw - ext4 /dev/sda3 rw
39 36 8:4 / /mnt/root/sub2 rw - ext4 /dev/sda4 rw";

        let result = parse_submounts(mountinfo, "/mnt/root");
        assert_eq!(
            result,
            vec![
                "/mnt/root",
                "/mnt/root/sub1",
                "/mnt/root/sub2",
                "/mnt/root/sub1/deep",
            ]
        );
    }

    #[test]
    fn test_parse_submounts_with_escaped_paths() {
        let mountinfo = "36 1 8:1 / /mnt/my\\040dir rw - ext4 /dev/sda1 rw\n\
37 36 8:2 / /mnt/my\\040dir/child rw - ext4 /dev/sda2 rw";

        let result = parse_submounts(mountinfo, "/mnt/my dir");
        assert_eq!(result, vec!["/mnt/my dir", "/mnt/my dir/child"]);
    }

    #[test]
    fn test_parse_submounts_skips_malformed_lines() {
        let mountinfo = "this is too short\n\
36 1 8:1 / /valid rw - ext4 /dev/sda1 rw\n\
\n\
37";

        let result = parse_submounts(mountinfo, "/valid");
        assert_eq!(result, vec!["/valid"]);
    }
}
