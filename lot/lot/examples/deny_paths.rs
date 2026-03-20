//! Demonstrate deny-path carve-outs from granted paths.
//!
//! Grants read access to a parent directory but denies a subdirectory within it.
//! The sandboxed process can read the parent but not the denied subtree.
//!
//! Run with: `cargo run --example deny_paths`

use std::fs;

use lot::{SandboxCommand, SandboxPolicyBuilder, spawn};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let parent = std::env::current_dir()?.join("lot_deny_example");
    let allowed = parent.join("allowed");
    let secrets = parent.join("secrets");

    fs::create_dir_all(&allowed)?;
    fs::create_dir_all(&secrets)?;
    fs::write(allowed.join("data.txt"), "public data")?;
    fs::write(secrets.join("key.txt"), "secret key")?;

    let policy = SandboxPolicyBuilder::new()
        .include_platform_exec_paths()
        .include_platform_lib_paths()
        .read_path(&parent)
        .deny_path(&secrets)
        .allow_network(false)
        .build()?;

    println!("Policy built. Deny path: {}", secrets.display());
    println!("Grant paths: {:?}", policy.read_paths());
    println!("Deny paths: {:?}", policy.deny_paths());

    // List the allowed directory (should succeed).
    #[cfg(unix)]
    let cmd = {
        let mut c = SandboxCommand::new("/bin/ls");
        c.arg(allowed.to_str().ok_or("non-utf8 path")?);
        c
    };
    #[cfg(windows)]
    let cmd = {
        let mut c = SandboxCommand::new("cmd.exe");
        c.args(["/C", &format!("dir {}", allowed.display())]);
        c.forward_common_env();
        c
    };

    let child = spawn(&policy, &cmd)?;
    let output = child.wait_with_output()?;
    println!(
        "\nListing allowed dir:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    // List the denied directory (should fail).
    #[cfg(unix)]
    let cmd2 = {
        let mut c = SandboxCommand::new("/bin/ls");
        c.arg(secrets.to_str().ok_or("non-utf8 path")?);
        c
    };
    #[cfg(windows)]
    let cmd2 = {
        let mut c = SandboxCommand::new("cmd.exe");
        c.args(["/C", &format!("dir {}", secrets.display())]);
        c.forward_common_env();
        c
    };

    let child2 = spawn(&policy, &cmd2)?;
    let output2 = child2.wait_with_output()?;
    println!("Listing denied dir (should fail):");
    println!("exit status: {}", output2.status);
    println!("stderr: {}", String::from_utf8_lossy(&output2.stderr));

    let _ = fs::remove_dir_all(&parent);
    Ok(())
}
