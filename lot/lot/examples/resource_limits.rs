//! Demonstrate resource limits: memory, process count, and CPU time.
//!
//! Shows how to configure resource constraints via `SandboxPolicyBuilder`.
//! The sandboxed process runs under the specified limits.
//!
//! Run with: `cargo run --example resource_limits`

use lot::{SandboxCommand, SandboxPolicyBuilder, spawn};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = SandboxPolicyBuilder::new()
        .include_platform_exec_paths()
        .include_platform_lib_paths()
        .include_temp_dirs()
        .allow_network(false)
        .max_memory_bytes(64 * 1024 * 1024) // 64 MB
        .max_processes(4)
        .max_cpu_seconds(10)
        .build()?;

    println!("Resource limits:");
    println!("  max_memory_bytes: {:?}", policy.limits().max_memory_bytes);
    println!("  max_processes:    {:?}", policy.limits().max_processes);
    println!("  max_cpu_seconds:  {:?}", policy.limits().max_cpu_seconds);

    #[cfg(unix)]
    let cmd = {
        let mut c = SandboxCommand::new("/bin/echo");
        c.arg("running under resource limits");
        c
    };
    #[cfg(windows)]
    let cmd = {
        let mut c = SandboxCommand::new("cmd.exe");
        c.args(["/C", "echo running under resource limits"]);
        c.forward_common_env();
        c
    };

    let child = spawn(&policy, &cmd)?;
    let output = child.wait_with_output()?;

    println!("\nexit status: {}", output.status);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    Ok(())
}
