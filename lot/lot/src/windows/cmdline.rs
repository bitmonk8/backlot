use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

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

const WIDE_SPACE: u16 = b' ' as u16;
const WIDE_TAB: u16 = b'\t' as u16;
const WIDE_QUOTE: u16 = b'"' as u16;
const WIDE_BACKSLASH: u16 = b'\\' as u16;

/// Escape an argument following `CommandLineToArgvW` rules:
/// - If the arg contains spaces, tabs, or quotes, wrap in quotes.
/// - Inside quotes, backslashes before a quote must be doubled, and quotes become `\"`.
///
/// Operates on UTF-16 code units directly to avoid `to_string_lossy()` corruption
/// of arguments containing unpaired surrogates.
fn append_escaped_arg(cmd: &mut OsString, arg: &OsString) {
    let wide: Vec<u16> = arg.as_os_str().encode_wide().collect();
    let needs_quoting = wide.is_empty()
        || wide
            .iter()
            .any(|&c| c == WIDE_SPACE || c == WIDE_TAB || c == WIDE_QUOTE);

    if !needs_quoting {
        cmd.push(arg);
        return;
    }

    cmd.push("\"");

    // Batch contiguous non-special code units to avoid per-unit allocation
    let mut plain = Vec::<u16>::new();
    let mut backslash_count: usize = 0;

    let flush_plain = |cmd: &mut OsString, plain: &mut Vec<u16>| {
        if !plain.is_empty() {
            cmd.push(OsString::from_wide(plain));
            plain.clear();
        }
    };

    for &unit in &wide {
        if unit == WIDE_BACKSLASH {
            backslash_count += 1;
        } else if unit == WIDE_QUOTE {
            // Double the backslashes before a quote, then emit \"
            flush_plain(cmd, &mut plain);
            for _ in 0..backslash_count {
                cmd.push("\\");
            }
            backslash_count = 0;
            cmd.push("\\\"");
        } else {
            // Flush pending backslashes as-is
            for _ in 0..backslash_count {
                cmd.push("\\");
            }
            backslash_count = 0;
            plain.push(unit);
        }
    }
    flush_plain(cmd, &mut plain);
    // Double trailing backslashes before the closing quote
    for _ in 0..backslash_count {
        cmd.push("\\");
    }

    cmd.push("\"");
}
