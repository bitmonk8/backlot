use std::collections::BTreeMap;
use std::io;

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};

use crate::SandboxPolicy;

/// Check whether seccomp-BPF is available.
pub fn available() -> bool {
    // SAFETY: PR_GET_SECCOMP reads kernel config, no pointers involved,
    // cannot corrupt state.
    //
    // Return values:
    //   0  — seccomp is supported but currently disabled for this thread
    //   2  — seccomp is in filter mode (also means supported)
    //  -1 (EINVAL) — kernel does not support seccomp at all
    let rc = unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) };
    rc >= 0
}

/// Insert all syscall numbers from `nrs` into `rules` with an unconditional allow.
fn allow_syscalls(rules: &mut BTreeMap<i64, Vec<seccompiler::SeccompRule>>, nrs: &[i64]) {
    let allow = vec![];
    for &nr in nrs {
        rules.insert(nr, allow.clone());
    }
}

/// Build a seccomp-BPF filter from the given sandbox policy.
///
/// The filter defaults to `ERRNO(EPERM)` for any syscall not in the allowlist.
/// Allowed syscalls get `Allow`. Network syscalls are gated on `policy.allow_network`.
///
/// **Filesystem note:** This seccomp filter does NOT enforce filesystem path
/// restrictions. Filesystem syscalls (open, read, write, mkdir, etc.) are
/// allowed because seccomp operates on syscall numbers and arguments, not
/// paths. Path-based isolation is the responsibility of the mount namespace
/// layer. Seccomp provides defense-in-depth for syscall categories
/// that are orthogonal to path access — primarily network and dangerous
/// process-creation flags.
///
/// Supports x86_64 and aarch64. Shared syscalls are listed once; arch-specific
/// syscalls (legacy x86_64 variants, aarch64 *at-only variants) are added
/// conditionally via `#[cfg]`.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn build_filter(policy: &SandboxPolicy) -> io::Result<BpfProgram> {
    let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = BTreeMap::new();

    // --- Process lifecycle ---
    allow_syscalls(&mut rules, &[
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_wait4,
        libc::SYS_waitid,
        libc::SYS_clone,
        libc::SYS_clone3,
    ]);

    // --- Memory management ---
    allow_syscalls(&mut rules, &[
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mprotect,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_msync,
    ]);

    // --- File I/O on already-open FDs ---
    allow_syscalls(&mut rules, &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_lseek,
        libc::SYS_close,
        libc::SYS_dup,
        libc::SYS_dup3,
        libc::SYS_fcntl,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
    ]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_dup2]);

    // --- File open/create ---
    allow_syscalls(&mut rules, &[libc::SYS_openat]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_open]);

    // --- Directory ---
    allow_syscalls(&mut rules, &[
        libc::SYS_getdents64,
        libc::SYS_getcwd,
        libc::SYS_chdir,
        libc::SYS_fchdir,
    ]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_getdents]);

    // --- Process info ---
    allow_syscalls(&mut rules, &[
        libc::SYS_getpid,
        libc::SYS_getppid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_getgid,
        libc::SYS_geteuid,
        libc::SYS_getegid,
        libc::SYS_getresuid,
        libc::SYS_getresgid,
    ]);

    // --- Signals ---
    allow_syscalls(&mut rules, &[
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_tgkill,
        libc::SYS_tkill,
        libc::SYS_kill,
    ]);

    // --- Time ---
    allow_syscalls(&mut rules, &[
        libc::SYS_clock_gettime,
        libc::SYS_clock_getres,
        libc::SYS_clock_nanosleep,
        libc::SYS_gettimeofday,
    ]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_nanosleep]);

    // --- Scheduling ---
    allow_syscalls(&mut rules, &[libc::SYS_sched_yield, libc::SYS_sched_getaffinity]);

    // --- System info ---
    allow_syscalls(&mut rules, &[libc::SYS_uname, libc::SYS_sysinfo]);

    // --- Misc ---
    allow_syscalls(&mut rules, &[
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_futex,
        libc::SYS_rseq,
        libc::SYS_getrandom,
        libc::SYS_pipe2,
        libc::SYS_socketpair, // local IPC only (e.g., tokio signal handler)
        libc::SYS_ppoll,
        libc::SYS_pselect6,
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_pwait,
        libc::SYS_eventfd2,
        // prctl is allowed broadly: seccomp filters stack (new filters can
        // only be more restrictive), PR_SET_NO_NEW_PRIVS is already set
        // before the filter is loaded, and filtering individual prctl ops
        // via argument rules is possible but fragile across kernel versions.
        libc::SYS_prctl,
        libc::SYS_ioctl,
        libc::SYS_memfd_create,
        libc::SYS_flock,
    ]);
    // x86_64 has legacy variants absent on aarch64
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[
        libc::SYS_arch_prctl,
        libc::SYS_pipe,
        libc::SYS_poll,
        libc::SYS_select,
        libc::SYS_epoll_wait,
    ]);

    // --- Exec ---
    allow_syscalls(&mut rules, &[libc::SYS_execve, libc::SYS_execveat]);

    // --- Access checks ---
    allow_syscalls(&mut rules, &[libc::SYS_faccessat, libc::SYS_faccessat2]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_access]);

    // --- Stat / readlink ---
    allow_syscalls(&mut rules, &[libc::SYS_readlinkat]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[
        libc::SYS_stat,
        libc::SYS_lstat,
        libc::SYS_readlink,
    ]);

    // --- Directory ops ---
    allow_syscalls(&mut rules, &[
        libc::SYS_mkdirat,
        libc::SYS_unlinkat,
        libc::SYS_renameat,
        libc::SYS_renameat2,
    ]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[
        libc::SYS_mkdir,
        libc::SYS_rmdir,
        libc::SYS_unlink,
        libc::SYS_rename,
    ]);

    // --- File metadata ---
    allow_syscalls(&mut rules, &[
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_utimensat,
    ]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[
        libc::SYS_chmod,
        libc::SYS_chown,
    ]);

    // --- Write ops ---
    allow_syscalls(&mut rules, &[
        libc::SYS_ftruncate,
        libc::SYS_fallocate,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
    ]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_truncate]);

    // --- Link ops ---
    allow_syscalls(&mut rules, &[libc::SYS_linkat, libc::SYS_symlinkat]);
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(&mut rules, &[libc::SYS_link, libc::SYS_symlink]);

    // --- Network syscalls (conditional) ---
    if policy.allow_network {
        allow_syscalls(&mut rules, &[
            libc::SYS_socket,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_connect,
            libc::SYS_sendto,
            libc::SYS_recvfrom,
            libc::SYS_sendmsg,
            libc::SYS_recvmsg,
            libc::SYS_shutdown,
            libc::SYS_getsockname,
            libc::SYS_getpeername,
            libc::SYS_setsockopt,
            libc::SYS_getsockopt,
            libc::SYS_sendmmsg,
            libc::SYS_recvmmsg,
        ]);
    }

    #[cfg(target_arch = "x86_64")]
    let arch = TargetArch::x86_64;
    #[cfg(target_arch = "aarch64")]
    let arch = TargetArch::aarch64;

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
        arch,
    )
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

    let bpf: BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| {
        io::Error::new(io::ErrorKind::InvalidInput, e.to_string())
    })?;

    Ok(bpf)
}

/// Apply a compiled BPF filter to the current thread.
///
/// Sets `PR_SET_NO_NEW_PRIVS` (required before loading seccomp as unprivileged)
/// and then loads the filter via the seccomp syscall.
pub fn apply_filter(program: &BpfProgram) -> io::Result<()> {
    seccompiler::apply_filter(program).map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_wrap)]
mod tests {
    use super::*;

    #[test]
    fn seccomp_available_no_panic() {
        let _result = available();
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn build_filter_no_network() {
        let policy = SandboxPolicy {
            read_paths: vec![],
            write_paths: vec![],
            exec_paths: vec![],
            deny_paths: vec![],
            allow_network: false,
            limits: crate::ResourceLimits::default(),
        };
        let bpf = build_filter(&policy);
        assert!(bpf.is_ok(), "build_filter failed: {:?}", bpf.err());
        assert!(!bpf.unwrap().is_empty());
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn build_filter_with_network() {
        let policy = SandboxPolicy {
            read_paths: vec![],
            write_paths: vec![],
            exec_paths: vec![],
            deny_paths: vec![],
            allow_network: true,
            limits: crate::ResourceLimits::default(),
        };
        let bpf = build_filter(&policy);
        assert!(bpf.is_ok(), "build_filter failed: {:?}", bpf.err());
        assert!(!bpf.unwrap().is_empty());
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn build_filter_network_produces_larger_program() {
        let base_policy = SandboxPolicy {
            read_paths: vec![],
            write_paths: vec![],
            exec_paths: vec![],
            deny_paths: vec![],
            allow_network: false,
            limits: crate::ResourceLimits::default(),
        };
        let net_policy = SandboxPolicy {
            allow_network: true,
            ..base_policy.clone()
        };
        let base_bpf = build_filter(&base_policy).unwrap();
        let net_bpf = build_filter(&net_policy).unwrap();
        // Network-enabled filter has more rules, so more BPF instructions
        assert!(net_bpf.len() > base_bpf.len());
    }

    /// Helper: create a pipe using libc, returns (read_fd, write_fd).
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn make_pipe() -> (i32, i32) {
        let mut fds = [0i32; 2];
        // SAFETY: fds is a valid 2-element array
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        (fds[0], fds[1])
    }

    /// Helper: write bytes to an fd.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn write_fd(fd: i32, data: &[u8]) {
        // SAFETY: fd is valid, data pointer and len are correct
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        assert_eq!(n, data.len() as isize, "short write to fd {fd}");
    }

    /// Helper: read all available bytes from an fd into a String.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn read_fd_to_string(fd: i32) -> String {
        let mut buf = [0u8; 256];
        // SAFETY: fd is valid, buf pointer and len are correct
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// Integration test: fork, apply filter, verify getpid() still works.
    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_getpid() {
        let policy = SandboxPolicy {
            read_paths: vec![],
            write_paths: vec![],
            exec_paths: vec![],
            deny_paths: vec![],
            allow_network: false,
            limits: crate::ResourceLimits::default(),
        };
        let bpf = build_filter(&policy).unwrap();

        let (read_fd, write_fd_val) = make_pipe();

        // SAFETY: fork is safe here; child only calls async-signal-safe
        // functions before _exit.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(read_fd) };

            if apply_filter(&bpf).is_err() {
                write_fd(write_fd_val, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: getpid has no preconditions
            let my_pid = unsafe { libc::getpid() };
            if my_pid > 0 {
                write_fd(write_fd_val, b"OK");
            } else {
                write_fd(write_fd_val, b"GETPID_FAIL");
            }
            // SAFETY: closing fd before exit
            unsafe { libc::close(write_fd_val) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fd
        unsafe { libc::close(write_fd_val) };

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = read_fd_to_string(read_fd);
        // SAFETY: closing fd
        unsafe { libc::close(read_fd) };

        assert_eq!(result, "OK", "child reported: {result}");
    }

    /// Integration test: fork, apply filter with network denied, verify socket() returns EPERM.
    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_denies_socket_without_network() {
        let policy = SandboxPolicy {
            read_paths: vec![],
            write_paths: vec![],
            exec_paths: vec![],
            deny_paths: vec![],
            allow_network: false,
            limits: crate::ResourceLimits::default(),
        };
        let bpf = build_filter(&policy).unwrap();

        let (read_fd, write_fd_val) = make_pipe();

        // SAFETY: fork is safe here; child only calls async-signal-safe
        // functions before _exit.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(read_fd) };

            if apply_filter(&bpf).is_err() {
                write_fd(write_fd_val, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: syscall args are valid; we expect it to fail with EPERM
            let rc = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
            if rc == -1 {
                // SAFETY: reading thread-local errno
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EPERM {
                    write_fd(write_fd_val, b"EPERM");
                } else {
                    // Avoid format!/allocator in forked child — write errno
                    // as raw bytes to avoid allocator deadlock risk.
                    write_fd(write_fd_val, b"WRONG_ERRNO:");
                    let errno_bytes = (errno as u32).to_ne_bytes();
                    write_fd(write_fd_val, &errno_bytes);
                }
            } else {
                write_fd(write_fd_val, b"SOCKET_SUCCEEDED");
            }
            // SAFETY: closing fd before exit
            unsafe { libc::close(write_fd_val) };
            unsafe { libc::_exit(0) };
        }

        // Parent
        // SAFETY: closing unused fd
        unsafe { libc::close(write_fd_val) };

        let mut status: i32 = 0;
        // SAFETY: valid pid, valid pointer
        unsafe { libc::waitpid(pid, &raw mut status, 0) };

        let result = read_fd_to_string(read_fd);
        // SAFETY: closing fd
        unsafe { libc::close(read_fd) };

        assert_eq!(result, "EPERM", "child reported: {result}");
    }
}
