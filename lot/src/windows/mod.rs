mod appcontainer;
mod job;

use crate::{PlatformCapabilities, Result};

pub const fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: false,
        appcontainer: appcontainer::available(),
        job_objects: job::available(),
    }
}

pub fn cleanup_stale() -> Result<()> {
    appcontainer::cleanup_stale()
}
