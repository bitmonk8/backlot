//! Minimal sandbox example: build a policy, spawn `echo hello`, read output.
//!
//! Run with: `cargo run --example basic`

use lot::{SandboxCommand, SandboxPolicyBuilder, spawn};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let caps = lot::probe();
    println!("Platform capabilities: {caps:?}");

    let policy = SandboxPolicyBuilder::new()
        .include_platform_exec_paths()
        .include_platform_lib_paths()
        .allow_network(false)
        .build()?;

    #[cfg(unix)]
    let cmd = {
        let mut c = SandboxCommand::new("/bin/echo");
        c.arg("hello from sandbox");
        c
    };

    #[cfg(windows)]
    let cmd = {
        let mut c = SandboxCommand::new("cmd.exe");
        c.args(["/C", "echo hello from sandbox"]);
        c.forward_common_env();
        c
    };

    let child = spawn(&policy, &cmd)?;
    let output = child.wait_with_output()?;

    println!("exit status: {}", output.status);
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    Ok(())
}
