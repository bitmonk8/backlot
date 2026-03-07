/// Check whether cgroups v2 delegation is available for the current user.
pub fn available() -> bool {
    // TODO: check /sys/fs/cgroup mount and delegation
    false
}
