#![allow(unsafe_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{CloseHandle, FALSE, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};

use crate::Result;
use crate::error::SandboxError;

use super::sddl::{get_sddl, restore_sddl};

// ── Helpers ──────────────────────────────────────────────────────────

fn sentinel_dir() -> PathBuf {
    std::env::temp_dir()
}

fn sentinel_path(profile_name: &str) -> PathBuf {
    sentinel_dir().join(format!("lot-sentinel-{profile_name}.txt"))
}

/// Extract the PID from a profile name. Format: `lot-{pid}-{tick}-{qpc}-{seq}`.
fn pid_from_profile_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("lot-")?;
    let pid_str = rest.split('-').next()?;
    pid_str.parse().ok()
}

/// Check whether a process with the given PID is still running.
///
/// Returns `true` conservatively when `OpenProcess` fails with
/// `ERROR_ACCESS_DENIED` — the process exists but belongs to a
/// higher-privilege session we cannot open.
///
/// Note: PID reuse is still theoretically possible if a different process
/// reuses the PID and is still running. Full mitigation would require
/// creation-time comparison, which the sentinel format doesn't currently store.
fn is_process_alive(pid: u32) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    // SAFETY: Querying process existence. Handle closed immediately.
    // SYNCHRONIZE is required for WaitForSingleObject.
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, FALSE, pid) };
    if handle.is_null() {
        // ACCESS_DENIED means the process exists but we can't open it
        // (e.g., elevated process). Conservatively assume alive.
        return io::Error::last_os_error().raw_os_error() == Some(5);
    }
    // SAFETY: Valid handle from OpenProcess. Timeout=0 for non-blocking check.
    let wait_result = unsafe { WaitForSingleObject(handle, 0) };
    // SAFETY: Handle from OpenProcess.
    unsafe {
        CloseHandle(handle);
    }
    // WAIT_TIMEOUT means the process hasn't exited yet (still alive).
    // WAIT_OBJECT_0 means it has exited. Any other result is treated as dead.
    wait_result == WAIT_TIMEOUT
}

// ── Sentinel file ────────────────────────────────────────────────────

/// Line 1: profile name. Subsequent lines: `path\tSDDL`.
/// Paths are percent-encoded: tab (`%09`), newline (`%0A`), carriage return (`%0D`),
/// and percent (`%25`) characters are encoded to preserve the line format.
#[derive(Debug)]
pub struct SentinelFile {
    pub profile_name: String,
    pub entries: Vec<(PathBuf, String)>,
    file_path: PathBuf,
}

impl SentinelFile {
    pub fn new(profile_name: String) -> Self {
        let file_path = sentinel_path(&profile_name);
        Self {
            profile_name,
            entries: Vec::new(),
            file_path,
        }
    }

    /// Create a sentinel that writes to a custom directory. Used by tests
    /// to avoid the system temp directory.
    #[cfg(test)]
    pub fn with_dir(profile_name: String, dir: &Path) -> Self {
        let file_path = dir.join(format!("lot-sentinel-{profile_name}.txt"));
        Self {
            profile_name,
            entries: Vec::new(),
            file_path,
        }
    }

    pub fn add_entry(&mut self, path: PathBuf, sddl: String) {
        self.entries.push((path, sddl));
    }

    pub fn write(&self) -> io::Result<()> {
        let mut content = self.profile_name.clone();
        content.push('\n');
        for (p, sddl) in &self.entries {
            let path_str = p.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("path is not valid Unicode: {}", p.display()),
                )
            })?;
            content.push_str(&Self::encode_path_field(path_str));
            content.push('\t');
            content.push_str(sddl);
            content.push('\n');
        }
        fs::write(&self.file_path, content)
    }

    /// Percent-encode tab, newline, carriage return, and percent in the path field
    /// so they don't break the `path\tSDDL\n` line format.
    fn encode_path_field(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '%' => out.push_str("%25"),
                '\t' => out.push_str("%09"),
                '\n' => out.push_str("%0A"),
                '\r' => out.push_str("%0D"),
                other => out.push(other),
            }
        }
        out
    }

    fn decode_path_field(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                let hex: String = chars.by_ref().take(2).collect();
                match hex.as_str() {
                    "25" => out.push('%'),
                    "09" => out.push('\t'),
                    "0A" => out.push('\n'),
                    "0D" => out.push('\r'),
                    _ => {
                        out.push('%');
                        out.push_str(&hex);
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    pub fn read(sentinel: &Path) -> io::Result<Self> {
        let content = fs::read_to_string(sentinel)?;
        let mut lines = content.lines();
        let profile_name = lines
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty sentinel file"))?
            .to_owned();

        let mut entries = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((path_str, sddl)) = line.split_once('\t') {
                entries.push((
                    PathBuf::from(Self::decode_path_field(path_str)),
                    sddl.to_owned(),
                ));
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("malformed sentinel entry (no tab separator): {line}"),
                ));
            }
        }
        Ok(Self {
            profile_name,
            entries,
            file_path: sentinel.to_owned(),
        })
    }

    pub fn delete_file(&self) -> io::Result<()> {
        match fs::remove_file(&self.file_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

pub fn write_sentinel(profile_name: &str, paths: &[PathBuf]) -> io::Result<SentinelFile> {
    let mut sentinel = SentinelFile::new(profile_name.to_owned());
    for path in paths {
        let sddl = get_sddl(path)?;
        sentinel.add_entry(path.clone(), sddl);
    }
    sentinel.write()?;
    Ok(sentinel)
}

/// Restore ACLs from a sentinel file. Deletes the sentinel only when all
/// restorations succeed; on partial failure the sentinel is preserved so
/// `cleanup_stale()` can retry.
///
/// Does NOT delete the AppContainer profile — callers own that responsibility.
pub fn restore_acls_and_delete_sentinel(sentinel: &SentinelFile) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    for (path, sddl) in &sentinel.entries {
        if let Err(e) = restore_sddl(path, sddl) {
            errors.push(format!("{}: {e}", path.display()));
        }
    }

    // Only delete sentinel when ALL ACL restorations succeeded.
    // On partial failure the sentinel must survive so cleanup_stale()
    // can retry on the next call.
    if !errors.is_empty() {
        return Err(SandboxError::Cleanup(errors.join("; ")));
    }

    if let Err(e) = sentinel.delete_file() {
        return Err(SandboxError::Cleanup(format!("delete sentinel: {e}")));
    }

    Ok(())
}

/// Find stale sentinel files in a specific directory.
///
/// Returns parsed sentinels whose owning process is no longer alive.
/// Does NOT perform any cleanup — the caller orchestrates restore + profile deletion.
pub fn find_stale_sentinels_in(dir: &Path) -> Result<Vec<SentinelFile>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(SandboxError::Cleanup(format!(
                "cannot read sentinel directory {}: {e}",
                dir.display()
            )));
        }
    };

    let mut stale = Vec::new();

    for entry in entries {
        // Best-effort scan: one unreadable directory entry must not block
        // cleanup of other stale sentinels.
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("lot-sentinel-")
            || !std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        {
            continue;
        }
        match SentinelFile::read(&path) {
            Ok(sentinel) => {
                // Skip sentinels owned by a still-running process — they
                // are live, not stale. The owning process's Drop will
                // clean them up.
                if let Some(pid) = pid_from_profile_name(&sentinel.profile_name) {
                    if is_process_alive(pid) {
                        continue;
                    }
                }
                stale.push(sentinel);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Sentinel was cleaned up concurrently; skip it.
            }
            Err(_) => {
                // Unreadable or corrupted sentinel — skip rather than blocking
                // cleanup of other stale sentinels. One bad file must not
                // prevent progress on the rest. The file will be retried on
                // the next cleanup_stale() call.
            }
        }
    }

    Ok(stale)
}

/// Find stale sentinel files left by crashed sessions.
///
/// Returns parsed sentinels whose owning process is no longer alive.
/// Does NOT perform any cleanup — the caller orchestrates restore + profile deletion.
pub fn find_stale_sentinels() -> Result<Vec<SentinelFile>> {
    find_stale_sentinels_in(&sentinel_dir())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ── encode_path_field / decode_path_field ───────────────────────

    #[test]
    fn encode_decode_round_trip() {
        let cases = [
            "C:\\normal\\path",
            "path with\ttab",
            "path with\nnewline",
            "path with\r\nCRLF",
            "100% done",
            "%25 already encoded",
            "mixed\t%\n\r",
            "",
        ];
        for input in &cases {
            let encoded = SentinelFile::encode_path_field(input);
            let decoded = SentinelFile::decode_path_field(&encoded);
            assert_eq!(
                *input, decoded,
                "round-trip failed for {input:?} (encoded: {encoded:?})"
            );
        }
    }

    #[test]
    fn encode_escapes_special_chars() {
        assert_eq!(SentinelFile::encode_path_field("a\tb"), "a%09b");
        assert_eq!(SentinelFile::encode_path_field("a\nb"), "a%0Ab");
        assert_eq!(SentinelFile::encode_path_field("a\rb"), "a%0Db");
        assert_eq!(SentinelFile::encode_path_field("a%b"), "a%25b");
    }

    #[test]
    fn decode_passes_through_unknown_escapes() {
        // Unknown %XX sequences are preserved literally.
        assert_eq!(SentinelFile::decode_path_field("%ZZ"), "%ZZ");
        assert_eq!(SentinelFile::decode_path_field("%1"), "%1");
    }

    #[test]
    fn decode_truncated_percent_at_end() {
        // '%' at end of string with <2 chars remaining — preserved literally.
        assert_eq!(SentinelFile::decode_path_field("trail%"), "trail%");
        assert_eq!(SentinelFile::decode_path_field("trail%0"), "trail%0");
    }

    // ── SentinelFile write/read ─────────────────────────────────────

    use tempfile::TempDir;

    fn make_test_dir() -> TempDir {
        let test_tmp = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("test_tmp");
        std::fs::create_dir_all(&test_tmp).expect("create test_tmp");
        TempDir::new_in(&test_tmp).expect("create temp dir")
    }

    #[test]
    fn sentinel_write_read_round_trip() {
        let dir = make_test_dir();
        let path = dir.path().join("sentinel.txt");

        let content = "lot-1234-5678-90-0\nC:\\Users\\test\\dir\tD:(A;;FA;;;BA)\nC:\\path with spaces\\sub\tD:(A;;FR;;;WD)\n";
        std::fs::write(&path, content).expect("write");

        let read_back = SentinelFile::read(&path).expect("read");

        assert_eq!(read_back.profile_name, "lot-1234-5678-90-0");
        assert_eq!(read_back.entries.len(), 2);
        assert_eq!(read_back.entries[0].0, PathBuf::from(r"C:\Users\test\dir"));
        assert_eq!(read_back.entries[0].1, "D:(A;;FA;;;BA)");
        assert_eq!(
            read_back.entries[1].0,
            PathBuf::from(r"C:\path with spaces\sub")
        );
        assert_eq!(read_back.entries[1].1, "D:(A;;FR;;;WD)");
    }

    #[test]
    fn sentinel_round_trip_special_chars_in_path() {
        let dir = make_test_dir();
        let path = dir.path().join("sentinel.txt");

        // Paths with tab, newline, percent — exercises encode/decode through write/read.
        let content = "lot-42-0-0-0\nC:\\100%25 done\tD:(A;;FA;;;BA)\nC:\\has%09tab\tD:(A;;FR;;;WD)\nC:\\has%0Anewline\tD:(D;;FW;;;WD)\n";
        std::fs::write(&path, content).expect("write");

        let read_back = SentinelFile::read(&path).expect("read");

        assert_eq!(read_back.entries.len(), 3);
        assert_eq!(read_back.entries[0].0, PathBuf::from("C:\\100% done"));
        assert_eq!(read_back.entries[1].0, PathBuf::from("C:\\has\ttab"));
        assert_eq!(read_back.entries[2].0, PathBuf::from("C:\\has\nnewline"));
    }

    #[test]
    fn sentinel_read_malformed_entry_errors() {
        let dir = make_test_dir();
        let path = dir.path().join("sentinel.txt");
        std::fs::write(&path, "lot-999-0-0-0\nno_tab_here\n").expect("write");

        let err = SentinelFile::read(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("no tab separator"),
            "error should mention tab: {err}"
        );
    }

    #[test]
    fn sentinel_read_empty_file_errors() {
        let dir = make_test_dir();
        let path = dir.path().join("sentinel.txt");
        std::fs::write(&path, "").expect("write");

        let err = SentinelFile::read(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn sentinel_read_skips_blank_lines() {
        let dir = make_test_dir();
        let path = dir.path().join("sentinel.txt");
        std::fs::write(&path, "lot-1-2-3-0\n\nC:\\foo\tD:(A;;FA;;;BA)\n\n").expect("write");

        let sentinel = SentinelFile::read(&path).expect("read");
        assert_eq!(sentinel.entries.len(), 1);
        assert_eq!(sentinel.entries[0].0, PathBuf::from(r"C:\foo"));
    }

    // ── pid_from_profile_name ────────────────────────────────────────

    #[test]
    fn pid_from_profile_name_valid() {
        assert_eq!(pid_from_profile_name("lot-1234-5678-90-0"), Some(1234));
        assert_eq!(pid_from_profile_name("lot-0-0-0-0"), Some(0));
        assert_eq!(
            pid_from_profile_name("lot-4294967295-1-2-3"),
            Some(4_294_967_295)
        );
    }

    #[test]
    fn pid_from_profile_name_invalid() {
        assert_eq!(pid_from_profile_name("not-a-lot-name"), None);
        assert_eq!(pid_from_profile_name("lot-"), None);
        assert_eq!(pid_from_profile_name("lot-abc-1-2-3"), None);
        assert_eq!(pid_from_profile_name(""), None);
    }

    #[test]
    fn pid_from_profile_name_overflow() {
        // u32::MAX is 4294967295; one digit more overflows.
        assert_eq!(pid_from_profile_name("lot-99999999999-1-2-3"), None);
    }

    // ── is_process_alive ─────────────────────────────────────────────

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_process_alive(pid), "current process should be alive");
    }

    #[test]
    fn exited_process_is_not_alive() {
        let child = std::process::Command::new("cmd.exe")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        // Wait for it to finish.
        let _ = child.wait_with_output();
        assert!(!is_process_alive(pid), "exited process should not be alive");
    }

    #[test]
    fn nonexistent_pid_is_not_alive() {
        // PID 0 is the System Idle Process, which we can't open with
        // PROCESS_QUERY_LIMITED_INFORMATION from a non-elevated context.
        // Use a very high PID unlikely to exist.
        assert!(!is_process_alive(u32::MAX));
    }

    // ── SentinelFile with_dir / write / delete ──────────────────────

    #[test]
    fn sentinel_write_to_dir_round_trip() {
        let dir = make_test_dir();
        let mut sentinel = SentinelFile::with_dir("lot-42-0-0-0".to_owned(), dir.path());
        sentinel.add_entry(PathBuf::from(r"C:\100% done"), "D:(A;;FA;;;BA)".to_owned());
        sentinel.add_entry(PathBuf::from("C:\\has\ttab"), "D:(A;;FR;;;WD)".to_owned());
        sentinel.write().expect("write");

        let expected_path = dir.path().join("lot-sentinel-lot-42-0-0-0.txt");
        assert!(expected_path.exists(), "sentinel file should exist");

        let read_back = SentinelFile::read(&expected_path).expect("read");
        assert_eq!(read_back.profile_name, "lot-42-0-0-0");
        assert_eq!(read_back.entries.len(), 2);
        assert_eq!(read_back.entries[0].0, PathBuf::from(r"C:\100% done"));
        assert_eq!(read_back.entries[0].1, "D:(A;;FA;;;BA)");
        assert_eq!(read_back.entries[1].0, PathBuf::from("C:\\has\ttab"));
        assert_eq!(read_back.entries[1].1, "D:(A;;FR;;;WD)");
    }

    #[test]
    fn sentinel_delete_file_removes_file() {
        let dir = make_test_dir();
        let mut sentinel = SentinelFile::with_dir("lot-99-0-0-0".to_owned(), dir.path());
        sentinel.add_entry(PathBuf::from(r"C:\foo"), "D:(A;;FA;;;BA)".to_owned());
        sentinel.write().expect("write");

        let expected_path = dir.path().join("lot-sentinel-lot-99-0-0-0.txt");
        assert!(expected_path.exists(), "file should exist after write");

        sentinel.delete_file().expect("delete");
        assert!(!expected_path.exists(), "file should be gone after delete");
    }

    // ── find_stale_sentinels_in ─────────────────────────────────────

    #[test]
    fn find_stale_sentinels_in_finds_dead_process() {
        let dir = make_test_dir();
        // PID 999999999 is almost certainly not running.
        let mut sentinel = SentinelFile::with_dir("lot-999999999-0-0-0".to_owned(), dir.path());
        sentinel.add_entry(PathBuf::from(r"C:\foo"), "D:(A;;FA;;;BA)".to_owned());
        sentinel.write().expect("write");

        let stale = find_stale_sentinels_in(dir.path()).expect("find");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].profile_name, "lot-999999999-0-0-0");
    }

    #[test]
    fn find_stale_sentinels_in_skips_live_process() {
        let dir = make_test_dir();
        let pid = std::process::id();
        let profile = format!("lot-{pid}-0-0-0");
        let mut sentinel = SentinelFile::with_dir(profile, dir.path());
        sentinel.add_entry(PathBuf::from(r"C:\foo"), "D:(A;;FA;;;BA)".to_owned());
        sentinel.write().expect("write");

        let stale = find_stale_sentinels_in(dir.path()).expect("find");
        assert!(stale.is_empty(), "live process sentinel should be skipped");
    }

    #[test]
    fn find_stale_sentinels_in_skips_non_sentinel_files() {
        let dir = make_test_dir();
        // Write a file that doesn't match the sentinel naming pattern.
        std::fs::write(dir.path().join("not-a-sentinel.txt"), "hello").expect("write");
        std::fs::write(dir.path().join("lot-sentinel-missing-ext"), "hello").expect("write");

        let stale = find_stale_sentinels_in(dir.path()).expect("find");
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_sentinels_in_empty_dir() {
        let dir = make_test_dir();
        let stale = find_stale_sentinels_in(dir.path()).expect("find");
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_sentinels_in_skips_corrupt_sentinel() {
        let dir = make_test_dir();
        // File matches naming pattern but has a malformed entry (no tab separator).
        // SentinelFile::read will return InvalidData error, which find_stale_sentinels_in skips.
        std::fs::write(
            dir.path().join("lot-sentinel-lot-888888888-0-0-0.txt"),
            "lot-888888888-0-0-0\ncorrupt_line_no_tab\n",
        )
        .expect("write");

        let stale = find_stale_sentinels_in(dir.path()).expect("find");
        assert!(stale.is_empty(), "corrupt sentinel should be skipped");
    }

    #[test]
    fn find_stale_sentinels_in_treats_unparseable_profile_as_stale() {
        let dir = make_test_dir();
        // Profile name doesn't match lot-{pid}-... format, so pid_from_profile_name
        // returns None. The sentinel should be treated as stale (cleaned up).
        let content = "custom-profile-name\nC:\\foo\tD:(A;;FA;;;BA)\n";
        std::fs::write(
            dir.path().join("lot-sentinel-custom-profile-name.txt"),
            content,
        )
        .expect("write");

        let stale = find_stale_sentinels_in(dir.path()).expect("find");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].profile_name, "custom-profile-name");
    }

    #[test]
    fn find_stale_sentinels_in_nonexistent_dir() {
        let dir = make_test_dir();
        let nonexistent = dir.path().join("does-not-exist");
        let stale = find_stale_sentinels_in(&nonexistent).expect("find");
        assert!(stale.is_empty());
    }

    // ── write_sentinel ────────────────────────────────────────────

    #[test]
    fn write_sentinel_creates_file_with_sddl() {
        let dir = make_test_dir();
        let test_path = dir.path().join("testfile.txt");
        std::fs::write(&test_path, "data").expect("write test file");

        // Use with_dir to write sentinel to a test-local directory
        // instead of the system temp directory, avoiding interference
        // with cleanup_stale() tests.
        let mut sentinel = SentinelFile::with_dir("lot-77777-0-0-0".to_owned(), dir.path());
        let sddl = super::get_sddl(&test_path).expect("get_sddl");
        sentinel.add_entry(test_path.clone(), sddl);
        sentinel.write().expect("write sentinel");

        assert_eq!(sentinel.profile_name, "lot-77777-0-0-0");
        assert_eq!(sentinel.entries.len(), 1);
        assert_eq!(sentinel.entries[0].0, test_path);
        assert!(!sentinel.entries[0].1.is_empty(), "SDDL should be captured");
    }

    // ── restore_acls_and_delete_sentinel success path ────────────

    #[test]
    fn restore_acls_and_delete_sentinel_success_deletes_file() {
        let dir = make_test_dir();
        let test_path = dir.path().join("restorable.txt");
        std::fs::write(&test_path, "data").expect("write test file");

        // Use with_dir to avoid system temp directory.
        let mut sentinel = SentinelFile::with_dir("lot-88888-0-0-0".to_owned(), dir.path());
        let sddl = super::get_sddl(&test_path).expect("get_sddl");
        sentinel.add_entry(test_path, sddl);
        sentinel.write().expect("write sentinel");

        let expected_path = dir.path().join("lot-sentinel-lot-88888-0-0-0.txt");
        assert!(expected_path.exists(), "sentinel file should exist");

        // Restore should succeed because the path exists and SDDL is valid
        restore_acls_and_delete_sentinel(&sentinel).expect("restore should succeed");

        // Sentinel file should be deleted on success
        assert!(
            !expected_path.exists(),
            "sentinel file should be deleted after successful restore"
        );
    }

    #[test]
    fn restore_acls_and_delete_sentinel_preserves_file_on_failure() {
        let dir = make_test_dir();
        let mut sentinel = SentinelFile::with_dir("lot-999999999-0-0-0".to_owned(), dir.path());
        // Non-existent path so restore_sddl will fail.
        sentinel.add_entry(
            PathBuf::from(r"C:\nonexistent_lot_test_path_12345"),
            "D:(A;;FA;;;BA)".to_owned(),
        );
        sentinel.write().expect("write");

        let expected_path = dir.path().join("lot-sentinel-lot-999999999-0-0-0.txt");
        assert!(
            expected_path.exists(),
            "sentinel should exist before restore"
        );

        let result = restore_acls_and_delete_sentinel(&sentinel);
        assert!(result.is_err(), "should fail due to non-existent path");

        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("nonexistent_lot_test_path_12345"),
            "error should mention the failed path: {err_msg}"
        );

        // Sentinel must survive so cleanup_stale() can retry restoration.
        assert!(
            expected_path.exists(),
            "sentinel file must be preserved when ACL restore fails"
        );
    }
}
