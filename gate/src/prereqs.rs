//! Stage 0 -- prerequisite checks that block execution before any test stage.
//!
//! [`check_prerequisites`] verifies, in order:
//! 1. Every required backlot binary still exists (re-confirms what binary
//!    discovery already proved -- catches a binary that vanished between
//!    discovery and the first stage launch).
//! 2. `~/.flick/providers` exists and contains at least one file.
//! 3. `~/.flick/models` contains the three required aliases (`fast`,
//!    `balanced`, `strong`) as TOML files.
//! 4. `lot setup --check` exits 0.
//!
//! Each check that fails contributes a specific actionable message to
//! [`PrereqError::problems`]. The runner aggregates **all** problems on
//! one pass and surfaces them together, so the operator can fix the full
//! gap in one round of setup rather than discover-fix-rerun.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::run_command;
use crate::runner::BinaryPaths;
use crate::types::Stage;

/// Aggregate prerequisite-failure detail.
///
/// Constructed only when one or more checks fail; on success
/// [`check_prerequisites`] returns `Ok(())`. `problems` is non-empty
/// whenever this struct is materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrereqError {
    /// One human-readable line per failed check, in check order. Each
    /// line names the missing artifact and the command that fixes it.
    pub problems: Vec<String>,
}

impl std::fmt::Display for PrereqError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for p in &self.problems {
            writeln!(f, "  - {p}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PrereqError {}

/// The three model aliases gate's stage tests reference. Order matches
/// the spec's tier ordering (cheapest first); the alphabetical run of the
/// directory listing is irrelevant because we look up each alias by name.
const REQUIRED_MODEL_ALIASES: &[&str] = &["fast", "balanced", "strong"];

/// Wall-clock cap on the `lot setup --check` invocation. The check is a
/// pure local syscall pass on every platform we target; 30s is generous
/// for slow disks and offers a clear timeout if the lot binary deadlocks.
const LOT_SETUP_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Entry point: run every prerequisite check against the real
/// `~/.flick/` tree and the real lot binary.
///
/// Returns `Ok(())` only when **all** checks pass. On failure the
/// returned `PrereqError` lists every problem found in this single pass
/// (we never short-circuit on the first failure).
pub fn check_prerequisites(binaries: &BinaryPaths) -> Result<(), PrereqError> {
    let flick_dir = match flick_home_dir() {
        Some(d) => d,
        None => {
            return Err(PrereqError {
                problems: vec![
                    "could not determine home directory (HOME or USERPROFILE not set) -- \
                     ~/.flick/ providers and models cannot be located"
                        .into(),
                ],
            });
        }
    };
    let lot_bin = binaries.lot.clone();
    check_prerequisites_inner(
        binaries,
        &flick_dir.join("providers"),
        &flick_dir.join("models"),
        || lot_setup_check(&lot_bin),
    )
}

/// Testable core: takes the providers/models directory paths and a
/// closure for the lot setup check, so unit tests can exercise the
/// aggregation logic against a temp directory without ever spawning the
/// real lot binary.
pub(crate) fn check_prerequisites_inner(
    binaries: &BinaryPaths,
    providers_dir: &Path,
    models_dir: &Path,
    lot_check: impl FnOnce() -> Result<(), String>,
) -> Result<(), PrereqError> {
    let mut problems = Vec::new();

    // 1) Required binaries exist (re-confirm; discovery already passed
    //    but a binary may have been deleted or moved between then and now).
    for stage in Stage::all() {
        let p = binaries.for_stage(stage);
        if !p.is_file() {
            problems.push(format!(
                "required binary missing: {} -- run `cargo build` from the workspace root",
                p.display()
            ));
        }
    }

    // 2) ~/.flick/providers exists and is non-empty.
    if let Err(msg) = check_providers_dir(providers_dir) {
        problems.push(msg);
    }

    // 3) ~/.flick/models contains every required alias.
    problems.extend(check_models_dir(models_dir, REQUIRED_MODEL_ALIASES));

    // 4) lot setup --check passes.
    if let Err(msg) = lot_check() {
        problems.push(msg);
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(PrereqError { problems })
    }
}

fn check_providers_dir(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!(
            "{} does not exist -- run `flick provider add <name>` to register a provider",
            dir.display()
        ));
    }
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("could not read {}: {e}", dir.display()))?;
    // Per-entry I/O errors during enumeration are intentionally swallowed:
    // the only outcome that matters here is whether *any* file is present.
    // A user-visible permission problem would already surface as the outer
    // `read_dir` Err above; the empty-dir branch below covers the case where
    // every entry was unreadable.
    let any_file = entries.filter_map(Result::ok).any(|e| e.path().is_file());
    if !any_file {
        return Err(format!(
            "{} is empty -- run `flick provider add <name>` to register a provider",
            dir.display()
        ));
    }
    Ok(())
}

fn check_models_dir(dir: &Path, aliases: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        out.push(format!(
            "{} does not exist -- run `flick model add <alias>` to create the required aliases ({})",
            dir.display(),
            aliases.join(", ")
        ));
        return out;
    }
    for alias in aliases {
        let path = dir.join(format!("{alias}.toml"));
        if !path.is_file() {
            out.push(format!(
                "~/.flick/models missing alias '{alias}' (expected {}) -- run `flick model add {alias}` to create it",
                path.display()
            ));
        }
    }
    out
}

fn lot_setup_check(lot_bin: &Path) -> Result<(), String> {
    let lot_str = lot_bin.to_string_lossy().to_string();
    let res = run_command(
        &lot_str,
        &["setup", "--check"],
        None,
        &[],
        LOT_SETUP_CHECK_TIMEOUT,
    )
    .map_err(|e| {
        format!(
            "`lot setup --check` could not be invoked ({}): {e}",
            lot_bin.display()
        )
    })?;
    if res.exit_code == 0 {
        Ok(())
    } else {
        let stderr_trimmed = res.stderr.trim();
        let stderr_part = if stderr_trimmed.is_empty() {
            String::new()
        } else {
            format!(" -- stderr: {stderr_trimmed}")
        };
        Err(format!(
            "`lot setup --check` failed (exit {}){} -- on Windows, run `lot setup` from an Administrator terminal",
            res.exit_code, stderr_part
        ))
    }
}

/// Resolve the `~/.flick` directory by reading `USERPROFILE` (Windows)
/// or `HOME` (everywhere else) directly from the process environment.
///
/// We do not depend on the `home` or `dirs` crates because flick itself
/// uses the same direct-env-var approach (see
/// `flick/flick/src/provider_registry.rs::home_dir`); using the same
/// resolution keeps gate's view of `~/.flick/` consistent with the
/// directory flick actually writes to.
fn flick_home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key).map(|s| PathBuf::from(s).join(".flick"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{BinaryPaths, binary_filename};
    use crate::scratch::scratch_base;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Project-local scratch dir for one test, under
    /// `target/gate-scratch/prereq-tests/`. Lives outside system temp to
    /// honor the workspace AppContainer rule (`CLAUDE.md`); functionally
    /// equivalent to `tempfile::TempDir` for testing purposes.
    struct TestDir(PathBuf);
    impl TestDir {
        fn new(label: &str) -> Self {
            let id = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = scratch_base()
                .join("prereq-tests")
                .join(format!("{label}-{pid}-{id}"));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch_dummy_binaries(dir: &Path) -> BinaryPaths {
        fs::create_dir_all(dir).expect("mkdir");
        for n in ["flick", "lot", "reel", "vault", "epic", "mech"] {
            let p = dir.join(binary_filename(n));
            fs::write(&p, b"").expect("touch binary");
        }
        BinaryPaths {
            flick: dir.join(binary_filename("flick")),
            lot: dir.join(binary_filename("lot")),
            reel: dir.join(binary_filename("reel")),
            vault: dir.join(binary_filename("vault")),
            epic: dir.join(binary_filename("epic")),
            mech: dir.join(binary_filename("mech")),
        }
    }

    fn write_providers(dir: &Path) {
        fs::create_dir_all(dir).expect("mkdir providers");
        fs::write(dir.join("anthropic"), b"key = \"x\"").expect("write provider");
    }

    fn write_aliases(dir: &Path, names: &[&str]) {
        fs::create_dir_all(dir).expect("mkdir models");
        for n in names {
            fs::write(
                dir.join(format!("{n}.toml")),
                b"provider = \"x\"\nname = \"y\"\n",
            )
            .expect("write alias");
        }
    }

    #[test]
    fn prereq_all_ok() {
        let td = TestDir::new("all-ok");
        let bins = touch_dummy_binaries(&td.path().join("bins"));
        let providers = td.path().join("providers");
        let models = td.path().join("models");
        write_providers(&providers);
        write_aliases(&models, &["fast", "balanced", "strong"]);

        let r = check_prerequisites_inner(&bins, &providers, &models, || Ok(()));
        assert!(r.is_ok(), "expected Ok, got {r:?}");
    }

    #[test]
    fn prereq_missing_providers() {
        let td = TestDir::new("no-providers");
        let bins = touch_dummy_binaries(&td.path().join("bins"));
        let providers = td.path().join("providers"); // not created
        let models = td.path().join("models");
        write_aliases(&models, &["fast", "balanced", "strong"]);

        let err = check_prerequisites_inner(&bins, &providers, &models, || Ok(()))
            .expect_err("missing providers dir");
        assert_eq!(
            err.problems.len(),
            1,
            "expected exactly one problem, got {:?}",
            err.problems
        );
        let p = &err.problems[0];
        assert!(
            p.contains("providers") && p.contains("does not exist"),
            "expected providers-missing message, got: {p}"
        );
        assert!(
            p.contains("flick provider add"),
            "expected actionable hint, got: {p}"
        );
    }

    #[test]
    fn prereq_missing_providers_when_dir_empty() {
        // Empty providers dir is treated identically to "missing" --
        // both fail with an actionable message naming the fix command.
        let td = TestDir::new("empty-providers");
        let bins = touch_dummy_binaries(&td.path().join("bins"));
        let providers = td.path().join("providers");
        fs::create_dir_all(&providers).unwrap(); // exists but empty
        let models = td.path().join("models");
        write_aliases(&models, &["fast", "balanced", "strong"]);

        let err = check_prerequisites_inner(&bins, &providers, &models, || Ok(()))
            .expect_err("empty providers dir");
        assert_eq!(err.problems.len(), 1, "{:?}", err.problems);
        assert!(
            err.problems[0].contains("is empty"),
            "got: {}",
            err.problems[0]
        );
    }

    #[test]
    fn prereq_missing_model_alias() {
        let td = TestDir::new("no-strong");
        let bins = touch_dummy_binaries(&td.path().join("bins"));
        let providers = td.path().join("providers");
        let models = td.path().join("models");
        write_providers(&providers);
        write_aliases(&models, &["fast", "balanced"]); // strong absent

        let err = check_prerequisites_inner(&bins, &providers, &models, || Ok(()))
            .expect_err("strong missing");
        assert_eq!(err.problems.len(), 1, "{:?}", err.problems);
        let p = &err.problems[0];
        assert!(p.contains("'strong'"), "expected strong-missing, got: {p}");
        assert!(
            !p.contains("'fast'") && !p.contains("'balanced'"),
            "should not mention present aliases, got: {p}"
        );
        assert!(
            p.contains("flick model add strong"),
            "expected actionable hint, got: {p}"
        );
    }

    #[test]
    fn prereq_multiple_problems() {
        let td = TestDir::new("multi");
        let bins = touch_dummy_binaries(&td.path().join("bins"));
        let providers = td.path().join("providers"); // not created
        let models = td.path().join("models"); // not created

        let err = check_prerequisites_inner(&bins, &providers, &models, || {
            Err("`lot setup --check` failed (exit 1) -- run `lot setup`".to_string())
        })
        .expect_err("everything missing");

        // Providers (1) + models-dir-missing (1) + lot-check (1) == 3.
        assert!(
            err.problems.len() >= 3,
            "expected >=3 problems, got {:?}",
            err.problems
        );
        assert!(
            err.problems.iter().any(|p| p.contains("providers")),
            "{:?}",
            err.problems
        );
        assert!(
            err.problems.iter().any(|p| p.contains("models")),
            "{:?}",
            err.problems
        );
        assert!(
            err.problems.iter().any(|p| p.contains("lot setup")),
            "{:?}",
            err.problems
        );
    }

    #[test]
    fn prereq_missing_binary_reported() {
        // A binary that vanished between discovery and the prereq check
        // (e.g., user ran `cargo clean` mid-flight) should be surfaced.
        let td = TestDir::new("no-binary");
        let bins = touch_dummy_binaries(&td.path().join("bins"));
        // Remove one binary post-discovery.
        fs::remove_file(&bins.flick).expect("remove flick binary");
        let providers = td.path().join("providers");
        let models = td.path().join("models");
        write_providers(&providers);
        write_aliases(&models, &["fast", "balanced", "strong"]);

        let err = check_prerequisites_inner(&bins, &providers, &models, || Ok(()))
            .expect_err("flick removed");
        assert!(
            err.problems
                .iter()
                .any(|p| p.contains("required binary missing") && p.contains("flick")),
            "{:?}",
            err.problems
        );
    }
}
