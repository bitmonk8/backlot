mod cgroup;
mod namespace;
mod seccomp;

use crate::PlatformCapabilities;

pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: namespace::available(),
        seccomp: seccomp::available(),
        cgroups_v2: cgroup::available(),
        seatbelt: false,
        appcontainer: false,
        job_objects: false,
    }
}
