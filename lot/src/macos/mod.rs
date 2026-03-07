mod seatbelt;

use crate::PlatformCapabilities;

pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: seatbelt::available(),
        appcontainer: false,
        job_objects: false,
    }
}
