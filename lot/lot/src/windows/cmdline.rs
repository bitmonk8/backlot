use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use super::path_to_wide;

// ── Command-line building/quoting ────────────────────────────────────

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
    // Reuse path_to_wide which does the same OsStr -> null-terminated UTF-16 conversion.
    path_to_wide(std::path::Path::new(&cmd))
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
            for _ in 0..(backslash_count * 2) {
                cmd.push("\\");
            }
            backslash_count = 0;
            cmd.push("\\\"");
        } else {
            // Backslashes not followed by a quote are literal — add to plain
            // buffer to preserve ordering with surrounding characters.
            plain.extend(std::iter::repeat_n(WIDE_BACKSLASH, backslash_count));
            backslash_count = 0;
            plain.push(unit);
        }
    }
    flush_plain(cmd, &mut plain);
    // Double trailing backslashes before the closing quote — CommandLineToArgvW
    // treats N backslashes followed by a quote as N/2 backslashes + escaped quote,
    // so we must emit 2N to get N literal backslashes before the closing quote.
    for _ in 0..(backslash_count * 2) {
        cmd.push("\\");
    }

    cmd.push("\"");
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_env_block ──────────────────────────────────────────────

    #[test]
    fn build_env_block_normal_entries() {
        let env = vec![
            (OsString::from("KEY1"), OsString::from("value1")),
            (OsString::from("KEY2"), OsString::from("value2")),
        ];
        let block = build_env_block(&env);
        // Expected: KEY1=value1\0KEY2=value2\0\0
        let decoded = String::from_utf16_lossy(&block);
        assert!(decoded.contains("KEY1=value1"));
        assert!(decoded.contains("KEY2=value2"));
        // Must end with double null (last two u16 are both 0)
        assert_eq!(block[block.len() - 1], 0);
        assert_eq!(block[block.len() - 2], 0);
    }

    #[test]
    fn build_env_block_empty_env() {
        let env: Vec<(OsString, OsString)> = vec![];
        let block = build_env_block(&env);
        // Empty env block is just a single null terminator
        assert_eq!(block, vec![0u16]);
    }

    #[test]
    fn build_env_block_unicode_values() {
        let env = vec![(OsString::from("GREETING"), OsString::from("hello\u{1F389}"))];
        let block = build_env_block(&env);
        let decoded = String::from_utf16_lossy(&block);
        assert!(
            decoded.contains("GREETING=hello\u{1F389}"),
            "env block should contain unicode value: {decoded}"
        );
    }

    #[test]
    fn build_env_block_single_entry() {
        let env = vec![(OsString::from("A"), OsString::from("B"))];
        let block = build_env_block(&env);
        // A=B\0\0
        let expected: Vec<u16> = "A=B"
            .encode_utf16()
            .chain(std::iter::once(0))
            .chain(std::iter::once(0))
            .collect();
        assert_eq!(block, expected);
    }

    // ── existing tests ───────────────────────────────────────────────

    /// Decode a null-terminated wide string back to a Rust String for assertions.
    fn wide_to_string(wide: &[u16]) -> String {
        let without_null = wide.strip_suffix(&[0]).unwrap_or(wide);
        String::from_utf16_lossy(without_null)
    }

    fn escape_one(arg: &str) -> String {
        let mut cmd = OsString::new();
        append_escaped_arg(&mut cmd, &OsString::from(arg));
        cmd.to_string_lossy().into_owned()
    }

    #[test]
    fn test_simple_arg() {
        let result = escape_one("hello");
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_arg_with_spaces() {
        let result = escape_one("hello world");
        assert_eq!(result, "\"hello world\"");
    }

    #[test]
    fn test_arg_with_quotes() {
        let result = escape_one("say\"hi");
        assert_eq!(result, "\"say\\\"hi\"");
    }

    #[test]
    fn test_arg_with_backslashes_before_quote() {
        // Input: c:\path\"arg — the `\` before `"` must be doubled per
        // CommandLineToArgvW rules, plus the quote itself is escaped.
        let result = escape_one("c:\\path\\\"arg");
        assert_eq!(result, "\"c:\\path\\\\\\\"arg\"");
    }

    #[test]
    fn test_empty_arg() {
        let result = escape_one("");
        assert_eq!(result, "\"\"");
    }

    #[test]
    fn test_trailing_backslashes() {
        // Trailing backslashes before the closing quote must be doubled.
        let result = escape_one("path with\\");
        assert_eq!(result, "\"path with\\\\\"");
    }

    #[test]
    fn test_build_command_line() {
        let program = OsString::from("my program.exe");
        let args = vec![OsString::from("simple"), OsString::from("with space")];
        let wide = build_command_line(&program, &args);
        let result = wide_to_string(&wide);
        assert_eq!(result, "\"my program.exe\" simple \"with space\"");
    }

    #[test]
    fn test_arg_with_only_quotes() {
        let result = escape_one("\"\"\"");
        assert_eq!(result, "\"\\\"\\\"\\\"\"");
    }

    #[test]
    fn test_non_bmp_unicode() {
        // U+1F389 PARTY POPPER -- a non-BMP character requiring a surrogate pair in UTF-16.
        let arg = OsString::from("hello\u{1F389}world");
        let program = OsString::from("test.exe");
        let wide = build_command_line(&program, &[arg]);
        let result = wide_to_string(&wide);
        assert_eq!(result, "\"test.exe\" hello\u{1F389}world");
    }

    #[test]
    fn test_unpaired_surrogate() {
        use std::os::windows::ffi::OsStringExt;
        // A lone high surrogate (U+D800) -- invalid Unicode but valid in OsString on Windows.
        let arg = OsString::from_wide(&[0xD800]);
        let program = OsString::from("test.exe");
        let wide = build_command_line(&program, &[arg]);
        assert!(
            wide.contains(&0xD800),
            "output should contain the lone surrogate 0xD800"
        );
    }
}
