#![allow(unsafe_code)]

use std::ffi::CString;
use std::fs;
use std::io;
use std::path::Path;

use crate::SandboxPolicy;

/// Check whether unprivileged user namespaces are available.
pub fn available() -> bool {
    if is_apparmor_restricted() {
        return false;
    }

    fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone").map_or_else(
        // Sysctl absent — kernel may still support user namespaces.
        // Fall back to a runtime probe.
        |_| probe_clone_newuser(),
        |contents| contents.trim() == "1",
    )
}

/// AppArmor can block unprivileged user namespaces even when the kernel supports them.
fn is_apparmor_restricted() -> bool {
    fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .is_ok_and(|v| v.trim() == "1")
}

/// Fork a child that attempts `clone(CLONE_NEWUSER)` and immediately exits.
/// Returns `true` if the child succeeds.
///
/// The fork()+unshare()+_exit() probe uses only async-signal-safe functions
/// between fork and _exit, so it is safe even in multi-threaded contexts.
fn probe_clone_newuser() -> bool {
    // SAFETY: We fork, then the child calls `_exit` immediately.
    // Between fork and _exit in the child we only call async-signal-safe
    // functions (unshare, _exit). The parent only calls waitpid.
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return false;
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
            return false;
        }
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
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

/// Set up the mount namespace: tmpfs root, bind mounts per policy, pivot_root.
///
/// This must be called after `unshare(CLONE_NEWUSER | CLONE_NEWNS)`.
/// Uses only paths from `policy` plus essential system directories.
pub fn setup_mount_namespace(policy: &SandboxPolicy) -> io::Result<()> {
    // Include PID to avoid collisions between concurrent sandboxes
    // SAFETY: getpid has no preconditions
    let pid = unsafe { libc::getpid() };
    let new_root = format!("/tmp/lot-newroot-{pid}");

    // Helper to write diagnostic directly to fd 2 (raw, no buffering)
    #[cfg(test)]
    fn diag(msg: &str) {
        let bytes = msg.as_bytes();
        // SAFETY: fd 2 is stderr, bytes is valid
        unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
        unsafe { libc::write(2, b"\n".as_ptr().cast(), 1) };
    }

    // Ensure the mount point exists
    mkdir_p(&new_root).map_err(|e| {
        #[cfg(test)]
        {
            let msg = format!("[mount-ns] FAIL mkdir_p({new_root}): {e}");
            diag(&msg);
        }
        e
    })?;
    #[cfg(test)]
    diag("[mount-ns] mkdir_p OK");

    // Mount tmpfs as the new root
    mount_tmpfs(&new_root).map_err(|e| {
        #[cfg(test)]
        {
            let msg = format!("[mount-ns] FAIL mount_tmpfs({new_root}): {e}");
            diag(&msg);
        }
        e
    })?;
    #[cfg(test)]
    diag("[mount-ns] mount_tmpfs OK");

    // Ensure we have a private mount propagation so pivot_root works
    make_mount_private("/").map_err(|e| {
        #[cfg(test)]
        {
            let msg = format!("[mount-ns] FAIL make_mount_private(/): {e}");
            diag(&msg);
        }
        e
    })?;
    #[cfg(test)]
    diag("[mount-ns] make_mount_private(/) OK");

    make_mount_private(&new_root).map_err(|e| {
        #[cfg(test)]
        {
            let msg = format!("[mount-ns] FAIL make_mount_private({new_root}): {e}");
            diag(&msg);
        }
        e
    })?;
    #[cfg(test)]
    diag("[mount-ns] make_mount_private(new_root) OK");

    // Create essential directories in the new root
    mkdir_p(&format!("{new_root}/proc"))?;
    mkdir_p(&format!("{new_root}/dev"))?;
    mkdir_p(&format!("{new_root}/tmp"))?;
    #[cfg(test)]
    diag("[mount-ns] essential dirs OK");

    // Bind-mount essential system directories (dynamic linker, libraries).
    // These need execute permission so shared libraries can be loaded.
    for sys_path in &["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/lib32"] {
        if Path::new(sys_path).exists() {
            let dest = format!("{new_root}{sys_path}");
            mkdir_p(&dest)?;
            bind_mount_exec(sys_path, &dest).map_err(|e| {
                #[cfg(test)]
                {
                    let msg = format!("[mount-ns] FAIL bind_mount_exec({sys_path}): {e}");
                    diag(&msg);
                }
                e
            })?;
        }
    }
    #[cfg(test)]
    diag("[mount-ns] lib bind-mounts OK");

    // Bind-mount bin directories — executables need execute permission.
    for bin_path in &["/bin", "/usr/bin", "/sbin", "/usr/sbin"] {
        if Path::new(bin_path).exists() {
            let dest = format!("{new_root}{bin_path}");
            mkdir_p(&dest)?;
            bind_mount_exec(bin_path, &dest).map_err(|e| {
                #[cfg(test)]
                {
                    let msg = format!("[mount-ns] FAIL bind_mount_exec({bin_path}): {e}");
                    diag(&msg);
                }
                e
            })?;
        }
    }
    #[cfg(test)]
    diag("[mount-ns] bin bind-mounts OK");

    // Bind-mount /etc read-only (needed for ld.so.cache, nsswitch, etc.)
    if Path::new("/etc").exists() {
        let dest = format!("{new_root}/etc");
        mkdir_p(&dest)?;
        bind_mount_readonly("/etc", &dest).map_err(|e| {
            #[cfg(test)]
            {
                let msg = format!("[mount-ns] FAIL bind_mount_readonly(/etc): {e}");
                diag(&msg);
            }
            e
        })?;
    }
    #[cfg(test)]
    diag("[mount-ns] /etc bind-mount OK");

    // Policy-specified read-only paths
    for path in &policy.read_paths {
        if let Some(s) = path.to_str() {
            let dest = format!("{new_root}{s}");
            mkdir_p(&dest)?;
            bind_mount_readonly(s, &dest).map_err(|e| {
                #[cfg(test)]
                {
                    let msg = format!("[mount-ns] FAIL bind_mount_readonly({s}): {e}");
                    diag(&msg);
                }
                e
            })?;
        }
    }
    #[cfg(test)]
    diag("[mount-ns] policy read paths OK");

    // Policy-specified read-write paths
    for path in &policy.write_paths {
        if let Some(s) = path.to_str() {
            let dest = format!("{new_root}{s}");
            mkdir_p(&dest)?;
            bind_mount_readwrite(s, &dest)?;
        }
    }

    // Policy-specified exec paths (read-only, but allow exec)
    for path in &policy.exec_paths {
        if let Some(s) = path.to_str() {
            let dest = format!("{new_root}{s}");
            mkdir_p(&dest)?;
            bind_mount_exec(s, &dest)?;
        }
    }

    // Mount /proc inside the new root
    mount_proc(&format!("{new_root}/proc")).map_err(|e| {
        #[cfg(test)]
        {
            let msg = format!("[mount-ns] FAIL mount_proc: {e}");
            diag(&msg);
        }
        e
    })?;
    #[cfg(test)]
    diag("[mount-ns] /proc mount OK");

    // Create /dev/null, /dev/zero, and /dev/urandom via bind mount
    for dev in &["/dev/null", "/dev/zero", "/dev/urandom"] {
        create_dev_node(&new_root, dev).map_err(|e| {
            #[cfg(test)]
            {
                let msg = format!("[mount-ns] FAIL create_dev_node({dev}): {e}");
                diag(&msg);
            }
            e
        })?;
    }
    #[cfg(test)]
    diag("[mount-ns] dev nodes OK");

    // pivot_root into the new root
    do_pivot_root(&new_root).map_err(|e| {
        #[cfg(test)]
        {
            let msg = format!("[mount-ns] FAIL do_pivot_root: {e}");
            diag(&msg);
        }
        e
    })?;
    #[cfg(test)]
    diag("[mount-ns] pivot_root OK");

    Ok(())
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

/// Bind-mount `src` to `dst` read-only.
fn bind_mount_readonly(src: &str, dst: &str) -> io::Result<()> {
    let c_src = to_cstring(src)?;
    let c_dst = to_cstring(dst)?;

    // First: bind mount
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

    // Second: remount to add read-only and other flags
    // SAFETY: valid CString pointer, remount with restrictive flags
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            c_dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND
                | libc::MS_REMOUNT
                | libc::MS_RDONLY
                | libc::MS_NOSUID
                | libc::MS_NODEV
                | libc::MS_NOEXEC,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bind-mount `src` to `dst` read-write (still nosuid/nodev).
fn bind_mount_readwrite(src: &str, dst: &str) -> io::Result<()> {
    let c_src = to_cstring(src)?;
    let c_dst = to_cstring(dst)?;

    // SAFETY: valid CString pointers
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

    // Remount with nosuid/nodev but NOT read-only
    // SAFETY: valid CString pointer, remount flags
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            c_dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Bind-mount `src` to `dst` read-only but executable (no MS_NOEXEC).
fn bind_mount_exec(src: &str, dst: &str) -> io::Result<()> {
    let c_src = to_cstring(src)?;
    let c_dst = to_cstring(dst)?;

    // SAFETY: valid CString pointers
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

    // Remount read-only + nosuid + nodev, but NOT noexec
    // SAFETY: valid CString pointer, remount flags
    let rc = unsafe {
        libc::mount(
            std::ptr::null(),
            c_dst.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Mount proc filesystem at the given path.
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

/// Create a device node in the new root by bind-mounting from the host.
fn create_dev_node(new_root: &str, dev_path: &str) -> io::Result<()> {
    let dest = format!("{new_root}{dev_path}");

    // Create an empty file to use as a mount point
    let c_dest = to_cstring(&dest)?;
    // SAFETY: valid CString pointer, creating regular file with 0o644
    let fd = unsafe { libc::open(c_dest.as_ptr(), libc::O_CREAT | libc::O_WRONLY, 0o644) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is valid (checked above)
    unsafe { libc::close(fd) };

    let c_src = to_cstring(dev_path)?;
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
fn do_pivot_root(new_root: &str) -> io::Result<()> {
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
mod tests {
    use super::*;

    #[test]
    fn namespace_available_no_panic() {
        let _result = available();
    }
}
