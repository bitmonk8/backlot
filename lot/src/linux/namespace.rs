/// Check whether unprivileged user namespaces are available.
pub fn available() -> bool {
    // TODO: check /proc/sys/kernel/unprivileged_userns_clone
    false
}
