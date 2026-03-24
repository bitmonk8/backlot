use std::collections::BTreeMap;
use std::io;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

use crate::SandboxPolicy;

/// Check whether seccomp-BPF is available.
pub fn is_available() -> bool {
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

/// Build argument-filtered rules: allow a syscall only when the argument at
/// `arg_index` matches one of `allowed_values`.
fn argument_filtered_rules(arg_index: u8, allowed_values: &[u64]) -> io::Result<Vec<SeccompRule>> {
    let mut rules = Vec::with_capacity(allowed_values.len());
    for &val in allowed_values {
        let cond = SeccompCondition::new(arg_index, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, val)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let rule = SeccompRule::new(vec![cond])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        rules.push(rule);
    }
    Ok(rules)
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
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_exit,
            libc::SYS_exit_group,
            libc::SYS_wait4,
            libc::SYS_waitid,
            libc::SYS_clone,
            libc::SYS_clone3,
        ],
    );

    // --- Memory management ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_brk,
            libc::SYS_mmap,
            libc::SYS_munmap,
            libc::SYS_mprotect,
            libc::SYS_mremap,
            libc::SYS_madvise,
            libc::SYS_msync,
        ],
    );

    // --- File I/O on already-open FDs ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_readv,
            libc::SYS_writev,
            libc::SYS_pread64,
            libc::SYS_pwrite64,
            libc::SYS_lseek,
            libc::SYS_close,
            libc::SYS_close_range,
            libc::SYS_dup,
            libc::SYS_dup3,
            libc::SYS_fcntl,
            libc::SYS_fstat,
            libc::SYS_newfstatat,
            libc::SYS_statx,
        ],
    );

    // --- File open/create ---
    allow_syscalls(&mut rules, &[libc::SYS_openat]);

    // --- Directory ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_getdents64,
            libc::SYS_getcwd,
            libc::SYS_chdir,
            libc::SYS_fchdir,
        ],
    );

    // --- Process info ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_getpid,
            libc::SYS_getppid,
            libc::SYS_gettid,
            libc::SYS_getuid,
            libc::SYS_getgid,
            libc::SYS_geteuid,
            libc::SYS_getegid,
            libc::SYS_getresuid,
            libc::SYS_getresgid,
            libc::SYS_prlimit64,
        ],
    );

    // --- Signals ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_rt_sigaction,
            libc::SYS_rt_sigprocmask,
            libc::SYS_rt_sigreturn,
            libc::SYS_sigaltstack,
            libc::SYS_tgkill,
            libc::SYS_tkill,
            libc::SYS_kill,
        ],
    );

    // --- Time ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_clock_gettime,
            libc::SYS_clock_getres,
            libc::SYS_clock_nanosleep,
            libc::SYS_gettimeofday,
        ],
    );

    // --- Scheduling ---
    allow_syscalls(
        &mut rules,
        &[libc::SYS_sched_yield, libc::SYS_sched_getaffinity],
    );

    // --- System info ---
    allow_syscalls(&mut rules, &[libc::SYS_uname, libc::SYS_sysinfo]);

    // --- Misc ---
    allow_syscalls(
        &mut rules,
        &[
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
            libc::SYS_memfd_create,
            libc::SYS_flock,
        ],
    );

    // --- prctl (argument-filtered on arg0) ---
    // Only allow specific prctl operations needed by standard runtimes.
    {
        const PR_SET_PDEATHSIG: u64 = 1;
        const PR_GET_PDEATHSIG: u64 = 2;
        const PR_SET_NAME: u64 = 15;
        const PR_GET_NAME: u64 = 16;
        const PR_SET_TIMERSLACK: u64 = 29;
        const PR_GET_TIMERSLACK: u64 = 30;

        let prctl_rules = argument_filtered_rules(
            0,
            &[
                PR_SET_PDEATHSIG,
                PR_GET_PDEATHSIG,
                PR_SET_NAME,
                PR_GET_NAME,
                PR_SET_TIMERSLACK,
                PR_GET_TIMERSLACK,
            ],
        )?;
        rules.insert(libc::SYS_prctl, prctl_rules);
    }

    // --- ioctl (argument-filtered on arg1: request number) ---
    // Only allow terminal and fd-flag ioctls needed by standard runtimes.
    // Values from asm-generic/ioctls.h — same on x86_64 and aarch64.
    {
        const TCGETS: u64 = 0x5401; // get terminal attributes
        const TIOCGWINSZ: u64 = 0x5413; // get window size
        const TIOCGPGRP: u64 = 0x540F; // get process group
        const FIONREAD: u64 = 0x541B; // bytes available for reading
        const FIOCLEX: u64 = 0x5451; // set close-on-exec
        const FIONCLEX: u64 = 0x5450; // clear close-on-exec

        let ioctl_rules = argument_filtered_rules(
            1,
            &[TCGETS, TIOCGWINSZ, TIOCGPGRP, FIONREAD, FIOCLEX, FIONCLEX],
        )?;
        rules.insert(libc::SYS_ioctl, ioctl_rules);
    }

    // --- Exec ---
    allow_syscalls(&mut rules, &[libc::SYS_execve, libc::SYS_execveat]);

    // --- Access checks ---
    allow_syscalls(&mut rules, &[libc::SYS_faccessat, libc::SYS_faccessat2]);

    // --- Stat / readlink ---
    allow_syscalls(&mut rules, &[libc::SYS_readlinkat]);

    // --- Directory ops ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_mkdirat,
            libc::SYS_unlinkat,
            libc::SYS_renameat,
            libc::SYS_renameat2,
        ],
    );

    // --- File metadata ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_fchmod,
            libc::SYS_fchmodat,
            libc::SYS_fchown,
            libc::SYS_fchownat,
            libc::SYS_utimensat,
        ],
    );

    // --- Write ops ---
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_ftruncate,
            libc::SYS_fallocate,
            libc::SYS_fsync,
            libc::SYS_fdatasync,
        ],
    );

    // --- Link ops ---
    allow_syscalls(&mut rules, &[libc::SYS_linkat, libc::SYS_symlinkat]);

    // --- x86_64 legacy syscall variants absent on aarch64 ---
    #[cfg(target_arch = "x86_64")]
    allow_syscalls(
        &mut rules,
        &[
            libc::SYS_dup2,
            libc::SYS_open,
            libc::SYS_getdents,
            libc::SYS_nanosleep,
            libc::SYS_arch_prctl,
            libc::SYS_pipe,
            libc::SYS_poll,
            libc::SYS_select,
            libc::SYS_epoll_wait,
            libc::SYS_access,
            libc::SYS_stat,
            libc::SYS_lstat,
            libc::SYS_readlink,
            libc::SYS_mkdir,
            libc::SYS_rmdir,
            libc::SYS_unlink,
            libc::SYS_rename,
            libc::SYS_chmod,
            libc::SYS_chown,
            libc::SYS_truncate,
            libc::SYS_link,
            libc::SYS_symlink,
        ],
    );

    // --- Network syscalls (conditional) ---
    if policy.allow_network() {
        allow_syscalls(
            &mut rules,
            &[
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
            ],
        );
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
    use super::super::test_helpers::{make_pipe, read_fd_to_string, write_fd};
    use super::*;

    fn empty_policy(allow_network: bool) -> SandboxPolicy {
        SandboxPolicy::new(
            vec![],
            vec![],
            vec![],
            vec![],
            allow_network,
            crate::ResourceLimits::default(),
        )
    }

    #[test]
    fn seccomp_available_no_panic() {
        let _result = is_available();
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn build_filter_no_network() {
        let bpf = build_filter(&empty_policy(false));
        assert!(bpf.is_ok(), "build_filter failed: {:?}", bpf.err());
        assert!(!bpf.unwrap().is_empty());
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn build_filter_with_network() {
        let bpf = build_filter(&empty_policy(true));
        assert!(bpf.is_ok(), "build_filter failed: {:?}", bpf.err());
        assert!(!bpf.unwrap().is_empty());
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn build_filter_network_produces_larger_program() {
        let base_bpf = build_filter(&empty_policy(false)).unwrap();
        let net_bpf = build_filter(&empty_policy(true)).unwrap();
        // Network-enabled filter has more rules, so more BPF instructions
        assert!(net_bpf.len() > base_bpf.len());
    }

    /// Fork a child, apply a seccomp filter, run `child_body` in the child,
    /// and return the string the child wrote to the result pipe.
    ///
    /// `child_body` receives the BPF program and the write fd for reporting
    /// results. It must use only async-signal-safe operations and call
    /// `libc::_exit` when done.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn fork_with_seccomp(bpf: &BpfProgram, child_body: fn(&BpfProgram, i32) -> !) -> String {
        let (read_fd, write_fd_val) = make_pipe();

        // SAFETY: fork is safe; child only calls async-signal-safe functions.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");

        if pid == 0 {
            // SAFETY: closing unused fd
            unsafe { libc::close(read_fd) };
            child_body(bpf, write_fd_val);
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

        result
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_getpid() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: getpid has no preconditions
            let my_pid = unsafe { libc::getpid() };
            write_fd(w, if my_pid > 0 { b"OK" } else { b"GETPID_FAIL" });
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "child reported: {result}");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_pr_set_name() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // PR_SET_NAME (15) should be allowed
            let name = b"testname\0";
            // SAFETY: valid prctl call
            let rc = unsafe { libc::prctl(15, name.as_ptr()) };
            write_fd(w, if rc == 0 { b"OK" } else { b"FAIL" });
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "PR_SET_NAME should be allowed");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_denies_pr_set_dumpable() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // PR_SET_DUMPABLE (4) is NOT in the allowed list
            // SAFETY: valid prctl call; we expect EPERM
            let rc = unsafe { libc::prctl(4, 0) };
            let errno = if rc < 0 {
                // SAFETY: reading thread-local errno
                unsafe { *libc::__errno_location() }
            } else {
                0
            };
            if rc < 0 && errno == libc::EPERM {
                write_fd(w, b"OK");
            } else {
                write_fd(w, b"ALLOWED");
            }
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "PR_SET_DUMPABLE should be denied by seccomp");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_socket_with_network() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: syscall args are valid; we expect it to succeed
            let rc = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
            if rc >= 0 {
                // SAFETY: close the socket fd
                unsafe { libc::close(rc) };
                write_fd(w, b"OK");
            } else {
                write_fd(w, b"SOCKET_FAILED");
            }
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(true)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(
            result, "OK",
            "socket should be allowed with network: {result}"
        );
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_ioctl_tcgets() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // TCGETS (0x5401) on stdin -- will fail with ENOTTY but should not
            // get EPERM from seccomp.
            let mut termios: libc::termios = unsafe { std::mem::zeroed() };
            // SAFETY: valid ioctl call, will return ENOTTY since stdin is a pipe
            let rc = unsafe { libc::ioctl(0, libc::TCGETS, &raw mut termios) };
            if rc < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EPERM {
                    write_fd(w, b"EPERM");
                } else {
                    // ENOTTY or similar is expected -- seccomp allowed it
                    write_fd(w, b"OK");
                }
            } else {
                write_fd(w, b"OK");
            }
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "TCGETS ioctl should be allowed: {result}");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_close_range() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: close_range on an empty range (FDs 10000..=10000) is harmless.
            let rc = unsafe { libc::syscall(libc::SYS_close_range, 10000u32, 10000u32, 0u32) };
            if rc < 0 {
                // SAFETY: reading thread-local errno
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EPERM {
                    write_fd(w, b"EPERM");
                } else {
                    // ENOSYS or EBADF are fine — seccomp allowed the call
                    write_fd(w, b"OK");
                }
            } else {
                write_fd(w, b"OK");
            }
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "close_range should be allowed: {result}");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_allows_prlimit64() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            let mut rlim: libc::rlimit64 = unsafe { std::mem::zeroed() };
            // SAFETY: prlimit64 with pid=0 (self), RLIMIT_NOFILE, no new limit,
            // valid output pointer.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_prlimit64,
                    0i32,
                    libc::RLIMIT_NOFILE as i32,
                    std::ptr::null::<libc::rlimit64>(),
                    &raw mut rlim,
                )
            };
            if rc < 0 {
                // SAFETY: reading thread-local errno
                let errno = unsafe { *libc::__errno_location() };
                if errno == libc::EPERM {
                    write_fd(w, b"EPERM");
                } else {
                    write_fd(w, b"FAIL");
                }
            } else {
                write_fd(w, b"OK");
            }
            unsafe { libc::close(w) };
            unsafe { libc::_exit(0) };
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "prlimit64 should be allowed: {result}");
    }

    /// Check that a syscall returned -1/EPERM and report via the pipe fd.
    /// Handles close + _exit so callers don't repeat that boilerplate.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn check_eperm_and_report(rc: isize, w: i32) -> ! {
        if rc < 0 {
            // SAFETY: reading thread-local errno
            let errno = unsafe { *libc::__errno_location() };
            if errno == libc::EPERM {
                write_fd(w, b"OK");
            } else {
                write_fd(w, b"WRONG_ERRNO");
            }
        } else {
            write_fd(w, b"SUCCEEDED");
        }
        // SAFETY: closing fd before exit
        unsafe { libc::close(w) };
        // SAFETY: async-signal-safe exit
        unsafe { libc::_exit(0) };
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_denies_disallowed_ioctl() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // TIOCSTI (0x5412) is NOT in the ioctl allowlist.
            // SAFETY: testing ioctl denial — we expect EPERM from seccomp
            // before the kernel inspects the fd or data pointer.
            let rc = unsafe { libc::ioctl(0, 0x5412 as libc::c_ulong, std::ptr::null::<u8>()) };
            check_eperm_and_report(rc.into(), w);
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "disallowed ioctl should get EPERM: {result}");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_denies_connect_without_network() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: connect with invalid fd; seccomp checks the syscall
            // number before fd validation, so we expect EPERM.
            let addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            let rc = unsafe {
                libc::connect(
                    -1,
                    &addr as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            };
            check_eperm_and_report(rc.into(), w);
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "connect should be denied without network: {result}");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_denies_sendto_without_network() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: sendto with invalid fd; seccomp rejects before fd check.
            let rc = unsafe {
                libc::sendto(
                    -1,
                    std::ptr::null(),
                    0,
                    0,
                    std::ptr::null(),
                    0,
                )
            };
            check_eperm_and_report(rc, w);
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "sendto should be denied without network: {result}");
    }

    #[test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    fn apply_filter_denies_socket_without_network() {
        fn child_body(bpf: &BpfProgram, w: i32) -> ! {
            if apply_filter(bpf).is_err() {
                write_fd(w, b"FILTER_FAIL");
                unsafe { libc::_exit(1) };
            }
            // SAFETY: syscall args are valid; we expect it to fail with EPERM
            let rc = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
            check_eperm_and_report(rc.into(), w);
        }

        let bpf = build_filter(&empty_policy(false)).unwrap();
        let result = fork_with_seccomp(&bpf, child_body);
        assert_eq!(result, "OK", "child reported: {result}");
    }
}
