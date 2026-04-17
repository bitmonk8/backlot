#![allow(dead_code)]
// Scaffolding: scratch helpers are exercised by tests now and consumed by stage modules added later.

//! Per-run scratch directory management.
//!
//! Each gate invocation creates a timestamped directory under
//! `target/gate-scratch/` containing per-stage subdirectories. Living under
//! the workspace `target/` keeps scratch paths project-local — required by
//! `lot`/`reel`/`epic` on Windows where AppContainer ancestor-traverse ACEs
//! cannot be granted under `%TEMP%` / `C:\Users` (see workspace `CLAUDE.md`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Subdirectories created eagerly inside every run dir, one per test stage
/// that needs filesystem scratch space.
const STAGE_SUBDIRS: &[&str] = &["lot", "reel", "vault", "epic"];

/// Workspace-local base path for all gate scratch dirs: `<workspace>/target/gate-scratch/`.
///
/// Derived from `CARGO_MANIFEST_DIR` (the `gate/` crate dir). The parent is
/// the workspace root, where `target/` lives.
pub fn scratch_base() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is the gate crate dir, and its parent is the
    // workspace root. `expect` rather than a silent fallback: a missing
    // parent would mean the crate dir is a filesystem root, which cannot
    // happen under any cargo invocation. Failing loudly catches the
    // misconfiguration immediately rather than producing a non-functional
    // `/target/gate-scratch` path.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .parent()
        .expect("CARGO_MANIFEST_DIR has no parent (cargo invariant violated)");
    workspace_root.join("target").join("gate-scratch")
}

/// Create a timestamped run directory with per-stage subdirectories.
///
/// Format: `target/gate-scratch/run-YYYYMMDD-HHMMSS/`. If a directory with
/// that exact timestamp already exists (consecutive invocations within the
/// same UTC second), a numeric suffix `-1`, `-2`, ... is appended until an
/// unused name is found. Stage subdirs (`lot/`, `reel/`, `vault/`, `epic/`)
/// are created eagerly, even for stages that may not run.
pub fn create_run_dir() -> io::Result<PathBuf> {
    let base = scratch_base();
    fs::create_dir_all(&base)?;
    let ts = format_timestamp_utc(SystemTime::now())?;
    // Race-safe pick: try mkdir directly and on AlreadyExists bump the
    // counter and retry. Naive exists()-then-mkdir loses to concurrent
    // callers (parallel test threads, parallel gate invocations) when they
    // hit the same one-second timestamp bucket.
    let mut counter: u32 = 0;
    let candidate = loop {
        let path = if counter == 0 {
            base.join(format!("run-{ts}"))
        } else {
            base.join(format!("run-{ts}-{counter}"))
        };
        match fs::create_dir(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                counter = counter
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("scratch run dir counter overflowed"))?;
                continue;
            }
            Err(e) => return Err(e),
        }
    };
    // Eager subdir creation. If any one fails (ACL / quota / disk full),
    // roll back the partially-populated run dir so the next invocation
    // does not accumulate orphans. Cleanup is best-effort: a rollback
    // failure is silently dropped because the caller will see the
    // original subdir-creation error first.
    for sub in STAGE_SUBDIRS {
        if let Err(e) = fs::create_dir(candidate.join(sub)) {
            let _ = cleanup_run_dir(&candidate);
            return Err(e);
        }
    }
    Ok(candidate)
}

/// Recursively delete a run directory. No-op if the path does not exist.
///
/// On Windows, `remove_dir_all` can transiently fail with
/// `PermissionDenied` when an AV scanner, search indexer, or another test
/// thread momentarily holds a handle on a freshly-created file. Retry a
/// few times with short backoff before giving up — matches the standard
/// workaround used by the `tempfile` crate.
pub fn cleanup_run_dir(path: &Path) -> io::Result<()> {
    const MAX_ATTEMPTS: u32 = 5;
    const BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);
    let mut last_err: Option<io::Error> = None;
    for _attempt in 0..MAX_ATTEMPTS {
        match fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                last_err = Some(e);
                std::thread::sleep(BACKOFF);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("cleanup_run_dir exhausted retries")))
}

/// Format a `SystemTime` as `YYYYMMDD-HHMMSS` in UTC.
fn format_timestamp_utc(t: SystemTime) -> io::Result<String> {
    // A clock set before 1970 is exotic (dead RTC, container with `date`
    // reset to epoch). Rather than silently producing a "19700101-000000"
    // dir name or panicking from a fallible call path, surface it as an
    // `io::Error` so `create_run_dir` returns `Err` and the runner can
    // present a clean error message.
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("system clock is before the Unix epoch"))?
        .as_secs();
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    Ok(format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}"))
}

/// Howard Hinnant's `civil_from_days` algorithm, plus time-of-day
/// extraction. Valid for any year in i64 range; we only feed it post-1970
/// epoch seconds, so the negative-`z` branches inside are unreachable.
///
/// Variable legend (matches the published reference at
/// <https://howardhinnant.github.io/date_algorithms.html>):
/// - `days` — count of days since 1970-01-01.
/// - `tod`  — seconds since midnight UTC (time-of-day).
/// - `z`    — `days` shifted so the era boundary aligns at 0000-03-01,
///   making the 400-year era's leap pattern uniform.
/// - `era`  — index of the 400-year era `z` falls in.
/// - `doe`  — day-of-era (0..146097).
/// - `yoe`  — year-of-era (0..400).
/// - `doy`  — day-of-year, March-anchored (March = 0).
/// - `mp`   — month-pulled, 0..12 with 0 = March, 11 = February.
fn epoch_to_ymdhms(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let tod = (secs % 86_400) as u32;
    let h = tod / 3_600;
    let mi = (tod / 60) % 60;
    let s = tod % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    /// All tests that call `create_run_dir` (or pre-create directories
    /// under `scratch_base()`) take this mutex so they run sequentially.
    /// The suffix-counter assertions need it for determinism; without it
    /// other tests that create their own `run-{ts}` dirs in parallel can
    /// claim the suffix slots the suffix tests are waiting for. The
    /// non-deterministic tests do not strictly need it, but the cost of
    /// taking it is negligible (<1ms each) and keeps the whole suite
    /// race-free on Windows where filesystem-handle contention also bites.
    static SCRATCH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Guard that removes a created run dir even when an assertion panics,
    /// so test failures don't leave stale state under `target/gate-scratch/`.
    struct CleanupGuard(PathBuf);
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            let _ = cleanup_run_dir(&self.0);
        }
    }

    fn run_dir_pattern_ok(name: &str) -> bool {
        // Expect "run-YYYYMMDD-HHMMSS" optionally followed by "-N".
        let Some(rest) = name.strip_prefix("run-") else {
            return false;
        };
        let mut parts = rest.splitn(3, '-');
        let date = parts.next().unwrap_or("");
        let time = parts.next().unwrap_or("");
        let suffix = parts.next();
        if date.len() != 8 || !date.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        if time.len() != 6 || !time.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        match suffix {
            None => true,
            Some(s) => !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()),
        }
    }

    #[test]
    fn scratch_base_under_workspace_target() {
        let base = scratch_base();
        let s = base.to_string_lossy().replace('\\', "/");
        assert!(
            s.ends_with("target/gate-scratch"),
            "base should end with target/gate-scratch, got {}",
            base.display()
        );
        // Must not be under system temp (a hard requirement from CLAUDE.md).
        let sys_temp = std::env::temp_dir();
        assert!(
            !base.starts_with(&sys_temp),
            "scratch base must not be under system temp ({}); got {}",
            sys_temp.display(),
            base.display()
        );
    }

    /// Parse the epoch seconds back out of a `run-YYYYMMDD-HHMMSS[-N]`
    /// directory name. Inverse of `format_timestamp_utc` for the test
    /// recency check; deliberately permissive — only the date/time pair
    /// matter, the optional suffix is ignored.
    fn parse_run_dir_name(name: &str) -> Option<u64> {
        let rest = name.strip_prefix("run-")?;
        let mut parts = rest.splitn(3, '-');
        let date = parts.next()?;
        let time = parts.next()?;
        if date.len() != 8 || time.len() != 6 {
            return None;
        }
        let y: i64 = date[0..4].parse().ok()?;
        let mo: u32 = date[4..6].parse().ok()?;
        let d: u32 = date[6..8].parse().ok()?;
        let h: u32 = time[0..2].parse().ok()?;
        let mi: u32 = time[2..4].parse().ok()?;
        let se: u32 = time[4..6].parse().ok()?;
        // Inverse Hinnant: days_from_civil. Same algorithm.
        let y = if mo <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as u64;
        let m = mo as u64;
        let dd = d as u64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + dd - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe as i64 - 719_468;
        let secs = (days as u64) * 86_400 + (h as u64) * 3_600 + (mi as u64) * 60 + (se as u64);
        Some(secs)
    }

    #[test]
    fn create_run_dir_exists() {
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let dir = create_run_dir().expect("create run dir");
        let _g = CleanupGuard(dir.clone());
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        assert!(dir.exists(), "run dir should exist: {}", dir.display());
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf-8 file name");
        assert!(
            run_dir_pattern_ok(name),
            "name should match run-YYYYMMDD-HHMMSS[-N], got {name}"
        );

        // Pin the embedded timestamp to "now". A 5-second window is wide
        // enough for any realistic create_run_dir runtime but tight enough
        // to catch sub-minute precision bugs (truncated seconds field,
        // off-by-one in the minute field) that a coarser slack hides.
        let parsed = parse_run_dir_name(name).unwrap_or_else(|| panic!("could not parse {name}"));
        assert!(
            parsed + 5 >= before && parsed <= after + 5,
            "run-dir timestamp {parsed} not within ±5s of now [{before}, {after}] (name {name})"
        );
    }

    #[test]
    fn create_run_dir_has_subdirs() {
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = create_run_dir().expect("create run dir");
        let _g = CleanupGuard(dir.clone());
        for sub in ["lot", "reel", "vault", "epic"] {
            let p = dir.join(sub);
            assert!(p.is_dir(), "expected subdir {} to exist", p.display());
        }
    }

    #[test]
    fn cleanup_removes_dir() {
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = create_run_dir().expect("create run dir");
        assert!(dir.exists());
        cleanup_run_dir(&dir).expect("cleanup");
        assert!(
            !dir.exists(),
            "dir should be gone after cleanup: {}",
            dir.display()
        );
    }

    #[test]
    fn cleanup_nonexistent_is_ok() {
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = scratch_base();
        let _ = fs::create_dir_all(&base);
        let p = base.join("does-not-exist-12345-gate-test");
        let _ = fs::remove_dir_all(&p);
        cleanup_run_dir(&p).expect("cleanup of missing path should be Ok");
    }

    #[test]
    fn cleanup_nonexistent_parent_is_ok() {
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Variant of `cleanup_nonexistent_is_ok` where not just the leaf
        // but the entire intermediate chain is missing. `remove_dir_all`
        // returns `NotFound` here too, but the suppression should cover it.
        let base = scratch_base();
        let p = base
            .join("never-created-parent-gate-test")
            .join("never-created-child");
        cleanup_run_dir(&p).expect("cleanup of missing parent chain should be Ok");
    }

    #[test]
    fn consecutive_runs_unique() {
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Two back-to-back runs must produce distinct paths regardless of
        // whether they fall in the same UTC second (suffix mechanism) or
        // straddle a second boundary (distinct timestamps). The suffix
        // path itself is exercised by the dedicated test below; this
        // test only pins the uniqueness contract.
        let a = create_run_dir().expect("create a");
        let _ga = CleanupGuard(a.clone());
        let b = create_run_dir().expect("create b");
        let _gb = CleanupGuard(b.clone());
        assert_ne!(a, b, "consecutive run dirs must be unique");
        assert!(a.exists());
        assert!(b.exists());
    }

    #[test]
    fn collision_suffix_increments_through_n() {
        // Pre-create `run-{ts}`, then verify two consecutive
        // `create_run_dir` calls return `run-{ts}-1` and `run-{ts}-2`
        // respectively. Single test (rather than two parallel ones)
        // because parallel tests racing on the same timestamp bucket
        // can produce flaky filesystem contention on Windows.
        //
        // A tiny race window exists if the wall clock advances into a new
        // second between the pre-create and the create_run_dir calls;
        // retry a few times to cover it.
        let _serialized = SCRATCH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let base = scratch_base();
        fs::create_dir_all(&base).expect("create base");
        for _attempt in 0..5 {
            let ts = format_timestamp_utc(SystemTime::now()).expect("timestamp");
            let bare = base.join(format!("run-{ts}"));
            if fs::create_dir(&bare).is_err() {
                let _ = fs::remove_dir_all(&bare);
                continue;
            }
            let _gbare = CleanupGuard(bare.clone());

            let first = match create_run_dir() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let _gfirst = CleanupGuard(first.clone());
            let first_name = first.file_name().unwrap().to_str().unwrap().to_string();
            if first_name != format!("run-{ts}-1") {
                // Slipped past the second; retry.
                continue;
            }

            let second = match create_run_dir() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let _gsecond = CleanupGuard(second.clone());
            let second_name = second.file_name().unwrap().to_str().unwrap();
            if second_name == format!("run-{ts}-2") {
                return;
            }
        }
        panic!("could not exercise -1 and -2 suffix paths within 5 attempts");
    }

    #[test]
    fn timestamp_format_unix_epoch() {
        assert_eq!(
            format_timestamp_utc(UNIX_EPOCH).expect("epoch is valid"),
            "19700101-000000"
        );
    }

    #[test]
    fn timestamp_format_known_date() {
        // 2026-04-17 14:30:52 UTC = 1_776_436_252 epoch seconds.
        let t = UNIX_EPOCH + Duration::from_secs(1_776_436_252);
        assert_eq!(
            format_timestamp_utc(t).expect("post-epoch is valid"),
            "20260417-143052"
        );
    }

    #[test]
    fn timestamp_format_leap_year() {
        // 2024-02-29 12:00:00 UTC = 1_709_208_000.
        let t = UNIX_EPOCH + Duration::from_secs(1_709_208_000);
        assert_eq!(
            format_timestamp_utc(t).expect("post-epoch is valid"),
            "20240229-120000"
        );
    }

    #[test]
    fn format_timestamp_utc_returns_err_for_pre_epoch_clock() {
        // SystemTime::UNIX_EPOCH - 1s is in the pre-epoch range.
        // `duration_since(UNIX_EPOCH)` returns Err for any earlier
        // SystemTime; the formatter must surface that as io::Error rather
        // than panic.
        let pre = UNIX_EPOCH - Duration::from_secs(1);
        let r = format_timestamp_utc(pre);
        assert!(r.is_err(), "expected Err for pre-epoch clock, got {r:?}");
    }

    // ---- parse_run_dir_name round-trip vectors ----
    //
    // Reuse the oracle values from `format_timestamp_utc` tests so a
    // bug in the inverse Hinnant arithmetic (especially around month/year
    // boundaries) is caught without depending on the date the test runs.

    #[test]
    fn parse_run_dir_name_unix_epoch() {
        assert_eq!(parse_run_dir_name("run-19700101-000000"), Some(0));
    }

    #[test]
    fn parse_run_dir_name_known_date() {
        assert_eq!(
            parse_run_dir_name("run-20260417-143052"),
            Some(1_776_436_252)
        );
    }

    #[test]
    fn parse_run_dir_name_leap_year() {
        assert_eq!(
            parse_run_dir_name("run-20240229-120000"),
            Some(1_709_208_000)
        );
    }

    #[test]
    fn parse_run_dir_name_with_suffix() {
        // The collision-suffix `-N` on race-retried names must not throw
        // off the parse — the date/time pair is what matters for the
        // recency check.
        assert_eq!(
            parse_run_dir_name("run-20260417-143052-3"),
            Some(1_776_436_252)
        );
    }

    #[test]
    fn parse_run_dir_name_rejects_bad_shapes() {
        assert_eq!(parse_run_dir_name("not-a-run-dir"), None);
        assert_eq!(parse_run_dir_name("run-2026041-143052"), None); // 7-digit date
        assert_eq!(parse_run_dir_name("run-20260417-14305"), None); // 5-digit time
        assert_eq!(parse_run_dir_name("run-abcdefgh-143052"), None);
    }
}
