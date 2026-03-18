use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;

// ── Command-line building/quoting ────────────────────────────────────

fn os_to_wide(s: &OsString) -> Vec<u16> {
    s.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn build_env_block(env: &[(OsString, OsString)]) -> Vec<u16> {
    let mut block = Vec::new();
    for (k, v) in env {
        block.extend(k.as_os_str().encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.as_os_str().encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

pub fn build_command_line(program: &OsString, args: &[OsString]) -> Vec<u16> {
    let mut cmd = OsString::new();
    cmd.push("\"");
    cmd.push(program);
    cmd.push("\"");
    for arg in args {
        cmd.push(" ");
        append_escaped_arg(&mut cmd, arg);
    }
    os_to_wide(&cmd)
}

/// Escape an argument following `CommandLineToArgvW` rules:
/// - If the arg contains spaces, tabs, or quotes, wrap in quotes.
/// - Inside quotes, backslashes before a quote must be doubled, and quotes become `\"`.
fn append_escaped_arg(cmd: &mut OsString, arg: &OsString) {
    let s = arg.to_string_lossy();
    let needs_quoting = s.is_empty() || s.contains(' ') || s.contains('\t') || s.contains('"');

    if !needs_quoting {
        cmd.push(arg);
        return;
    }

    cmd.push("\"");

    let mut backslash_count: usize = 0;
    for ch in s.chars() {
        if ch == '\\' {
            backslash_count += 1;
        } else if ch == '"' {
            // Double the backslashes before a quote, then emit \"
            for _ in 0..backslash_count {
                cmd.push("\\");
            }
            backslash_count = 0;
            cmd.push("\\\"");
        } else {
            backslash_count = 0;
            let mut buf = [0u8; 4];
            cmd.push(ch.encode_utf8(&mut buf) as &str);
        }
    }
    // Double trailing backslashes before the closing quote
    for _ in 0..backslash_count {
        cmd.push("\\");
    }

    cmd.push("\"");
}
