mod appcontainer;
mod job;

pub use appcontainer::WindowsSandboxedChild;

use crate::command::SandboxCommand;
use crate::policy::SandboxPolicy;
use crate::{PlatformCapabilities, Result, SandboxedChild};

#[allow(clippy::missing_const_for_fn)] // Matches non-const signature on other platforms.
pub fn probe() -> PlatformCapabilities {
    PlatformCapabilities {
        namespaces: false,
        seccomp: false,
        cgroups_v2: false,
        seatbelt: false,
        appcontainer: appcontainer::available(),
        job_objects: job::available(),
    }
}

pub fn spawn(policy: &SandboxPolicy, command: &SandboxCommand) -> Result<SandboxedChild> {
    appcontainer::spawn(policy, command)
}

pub fn cleanup_stale() -> Result<()> {
    appcontainer::cleanup_stale()
}
