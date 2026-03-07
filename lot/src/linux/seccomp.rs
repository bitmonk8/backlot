/// Check whether seccomp-BPF is available.
pub fn available() -> bool {
    // TODO: check prctl(PR_GET_SECCOMP)
    false
}
