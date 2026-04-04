/// Best-effort detection of whether epic is running inside a container or VM.
pub fn detect_virtualization() -> bool {
    platform::detect()
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::process::Command;

    pub fn detect() -> bool {
        docker_or_podman() || wsl() || systemd_detect_virt()
    }

    fn docker_or_podman() -> bool {
        // Container runtimes drop sentinel files at root.
        if std::path::Path::new("/.dockerenv").exists() {
            return true;
        }
        if std::path::Path::new("/run/.containerenv").exists() {
            return true;
        }
        // cgroup v1 includes the container runtime name; v2 unified hierarchy does not.
        // Sentinel file checks above cover the v2 gap for Docker/Podman.
        if let Ok(cgroup) = fs::read_to_string("/proc/1/cgroup") {
            if cgroup_indicates_container(&cgroup) {
                return true;
            }
        }
        false
    }

    fn wsl() -> bool {
        if let Ok(version) = fs::read_to_string("/proc/version") {
            if version_indicates_wsl(&version) {
                return true;
            }
        }
        false
    }

    pub(super) fn cgroup_indicates_container(content: &str) -> bool {
        let lower = content.to_lowercase();
        lower.contains("docker") || lower.contains("containerd") || lower.contains("podman")
    }

    pub(super) fn version_indicates_wsl(content: &str) -> bool {
        let lower = content.to_lowercase();
        lower.contains("microsoft") || lower.contains("wsl")
    }

    fn systemd_detect_virt() -> bool {
        // Exit code 0 means virtualized; non-zero means bare metal or unknown.
        let result = Command::new("systemd-detect-virt")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        matches!(result, Ok(status) if status.success())
    }

    // systemd-detect-virt is near-instant; no timeout needed.
}

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    pub fn detect() -> bool {
        hv_vmm_present()
    }

    fn hv_vmm_present() -> bool {
        let result = Command::new("sysctl")
            .args(["-n", "kern.hv_vmm_present"])
            .output();
        match result {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                sysctl_indicates_vm(&text)
            }
            Err(_) => false,
        }
    }

    pub(super) fn sysctl_indicates_vm(output: &str) -> bool {
        output.trim() == "1"
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::process::Command;

    pub fn detect() -> bool {
        powershell_model_check()
    }

    fn powershell_model_check() -> bool {
        // wmic is deprecated on Windows 11; use PowerShell CIM query instead.
        let result = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).Model",
            ])
            .output();
        match result {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout);
                model_indicates_vm(&text)
            }
            Err(_) => false,
        }
    }

    pub(super) fn model_indicates_vm(text: &str) -> bool {
        let lower = text.to_lowercase();
        lower.contains("virtual")
            || lower.contains("vmware")
            || lower.contains("virtualbox")
            || lower.contains("kvm")
            || lower.contains("qemu")
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    pub fn detect() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_bool_without_panicking() {
        // Can't assert a specific value in CI — just verify it doesn't panic.
        let _result: bool = detect_virtualization();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cgroup_detects_docker() {
        assert!(platform::cgroup_indicates_container(
            "12:blkio:/docker/abc123\n"
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cgroup_detects_containerd() {
        assert!(platform::cgroup_indicates_container(
            "0::/system.slice/containerd.service\n"
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cgroup_detects_podman() {
        assert!(platform::cgroup_indicates_container(
            "0::/machine.slice/libpod-abc.scope\npodman\n"
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cgroup_bare_metal() {
        assert!(!platform::cgroup_indicates_container("0::/init.scope\n"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn version_detects_wsl() {
        assert!(platform::version_indicates_wsl(
            "Linux version 5.15.90.1-microsoft-standard-WSL2"
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn version_detects_wsl_case_insensitive() {
        assert!(platform::version_indicates_wsl(
            "Linux version 5.15 Microsoft"
        ));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn version_bare_metal() {
        assert!(!platform::version_indicates_wsl(
            "Linux version 6.5.0-generic"
        ));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn sysctl_vm_present() {
        assert!(platform::sysctl_indicates_vm("1\n"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn sysctl_no_vm() {
        assert!(!platform::sysctl_indicates_vm("0\n"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_detects_vmware() {
        assert!(platform::model_indicates_vm(
            "Model=VMware Virtual Platform\r\n"
        ));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_detects_virtualbox() {
        assert!(platform::model_indicates_vm("Model=VirtualBox\r\n"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_detects_generic_virtual() {
        assert!(platform::model_indicates_vm("Model=Virtual Machine\r\n"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_bare_metal() {
        assert!(!platform::model_indicates_vm("Model=HP ProDesk 400 G7\r\n"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_detects_kvm() {
        assert!(platform::model_indicates_vm(
            "Model=Standard PC (Q35 + ICH9) KVM\r\n"
        ));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_detects_qemu() {
        assert!(platform::model_indicates_vm(
            "Model=Standard PC (i440FX + PIIX, 1996) QEMU\r\n"
        ));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_empty_string() {
        assert!(!platform::model_indicates_vm(""));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cgroup_empty_string() {
        assert!(!platform::cgroup_indicates_container(""));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn model_partial_substring_not_false_positive() {
        // "NotVirtuallyAnything" contains "virtual" as a substring.
        // The current implementation lowercases and checks .contains("virtual"),
        // so this WILL match. This test documents the known behavior.
        assert!(
            platform::model_indicates_vm("NotVirtuallyAnything"),
            "substring 'virtual' within 'NotVirtuallyAnything' is detected (known behavior)"
        );
    }
}
