// Bootstrap operation: converts initial requirements into the core document set.
//
// Runs once before any interactive session. Writes raw requirements to disk,
// invokes the librarian to produce derived documents, then records the
// operation in the changelog.

use crate::BootstrapError;
use crate::librarian::LibrarianInvoker;
use crate::prompts;
use crate::storage::{ChangelogEntry, DerivedValidationWarning, Storage, utc_now_iso8601};

/// Execute the bootstrap operation.
///
/// Pre-condition: storage must not be initialized (no `CHANGELOG.md`, `raw/`,
/// or `derived/`). Writes requirements to `raw/REQUIREMENTS_1.md`, invokes the
/// librarian to create derived documents, validates results, and appends a
/// changelog entry.
pub async fn run<L: LibrarianInvoker>(
    storage: &Storage,
    invoker: &L,
    requirements: &str,
) -> Result<Vec<DerivedValidationWarning>, BootstrapError> {
    // Pre-condition: reject if already initialized.
    if storage.is_initialized() {
        return Err(BootstrapError::AlreadyInitialized);
    }

    // Step 1: Create directories.
    storage.create_directories()?;

    // Step 2: Write raw requirements.
    let raw_filename = storage.write_raw_versioned("REQUIREMENTS", requirements, true)?;

    // Step 3: Build prompt and invoke librarian.
    let system_prompt = prompts::build_bootstrap_prompt(storage)?;
    let query = prompts::bootstrap_query();

    invoker
        .produce_derived(&system_prompt, query, storage)
        .await
        .map_err(BootstrapError::LibrarianFailed)?;

    // Step 4: Post-invocation validation (warnings only, returned to caller).
    let warnings = storage.validate_derived()?;

    // Step 5: Append changelog entry.
    let entry = ChangelogEntry::Bootstrap {
        ts: utc_now_iso8601(),
        raw: raw_filename,
    };
    storage.append_changelog(&entry)?;

    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;

    /// Mock librarian that writes minimal derived documents to simulate success.
    struct MockLibrarian {
        /// When true, the mock writes valid derived documents.
        succeed: bool,
        /// Track whether invoke was called.
        invoked: AtomicBool,
    }

    impl MockLibrarian {
        const fn succeeding() -> Self {
            Self {
                succeed: true,
                invoked: AtomicBool::new(false),
            }
        }

        const fn failing() -> Self {
            Self {
                succeed: false,
                invoked: AtomicBool::new(false),
            }
        }

        fn was_invoked(&self) -> bool {
            self.invoked.load(Ordering::Relaxed)
        }
    }

    impl LibrarianInvoker for MockLibrarian {
        async fn produce_derived(
            &self,
            _system_prompt: &str,
            _query: &str,
            storage: &Storage,
        ) -> Result<(), String> {
            self.invoked.store(true, Ordering::Relaxed);

            if !self.succeed {
                return Err("mock librarian failure".to_owned());
            }

            // Write minimal valid derived documents.
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project Overview\n<!-- scope: top-level project index -->\n\nProject content.\n",
            )
            .map_err(|e| e.to_string())?;
            fs::write(
                derived.join("REQUIREMENTS.md"),
                "# Requirements\n<!-- scope: structured requirements -->\n\nRequirements content.\n",
            )
            .map_err(|e| e.to_string())?;

            Ok(())
        }
    }

    /// Mock librarian that writes a file with a bad filename.
    struct BadNameLibrarian;

    impl LibrarianInvoker for BadNameLibrarian {
        async fn produce_derived(
            &self,
            _system_prompt: &str,
            _query: &str,
            storage: &Storage,
        ) -> Result<(), String> {
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project\n<!-- scope: overview -->\n",
            )
            .map_err(|e| e.to_string())?;
            // Bad filename: lowercase.
            fs::write(derived.join("bad_name.md"), "no header\n").map_err(|e| e.to_string())?;
            Ok(())
        }
    }

    /// Mock librarian that captures the system prompt and query for verification.
    struct CapturingLibrarian {
        captured_prompt: Mutex<Option<String>>,
        captured_query: Mutex<Option<String>>,
    }

    impl CapturingLibrarian {
        fn new() -> Self {
            Self {
                captured_prompt: Mutex::new(None),
                captured_query: Mutex::new(None),
            }
        }
    }

    impl LibrarianInvoker for CapturingLibrarian {
        async fn produce_derived(
            &self,
            system_prompt: &str,
            query: &str,
            storage: &Storage,
        ) -> Result<(), String> {
            *self.captured_prompt.lock().map_err(|e| e.to_string())? =
                Some(system_prompt.to_owned());
            *self.captured_query.lock().map_err(|e| e.to_string())? = Some(query.to_owned());

            // Write minimal valid derived documents so the operation can complete.
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project\n<!-- scope: overview -->\n",
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn bootstrap_passes_correct_prompt_and_query() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        let invoker = CapturingLibrarian::new();

        run(&storage, &invoker, "Test requirements.").await.unwrap();

        let prompt = invoker.captured_prompt.lock().unwrap().clone().unwrap();
        let query = invoker.captured_query.lock().unwrap().clone().unwrap();

        // Prompt must contain all shared blocks and the bootstrap-specific block.
        assert!(prompt.contains("Core Principle"), "missing Core Principle");
        assert!(prompt.contains("Bootstrap Task"), "missing Bootstrap Task");
        assert!(
            prompt.contains("raw/REQUIREMENTS_1.md"),
            "inventory should list the raw document written before invocation"
        );

        // Query should be the bootstrap query constant.
        assert_eq!(query, prompts::bootstrap_query());
    }

    #[tokio::test]
    async fn bootstrap_succeeds_on_fresh_storage() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        let invoker = MockLibrarian::succeeding();

        let result = run(&storage, &invoker, "Build a widget system.").await;
        assert!(result.is_ok());
        assert!(invoker.was_invoked());

        // Raw requirements written.
        let raw_content = storage.read_raw("REQUIREMENTS_1.md").unwrap();
        assert_eq!(raw_content, "Build a widget system.");

        // Derived documents exist.
        let derived = storage.list_derived().unwrap();
        assert!(derived.iter().any(|d| d.filename == "PROJECT.md"));
        assert!(derived.iter().any(|d| d.filename == "REQUIREMENTS.md"));

        // Changelog entry recorded.
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ChangelogEntry::Bootstrap { raw, .. } if raw == "REQUIREMENTS_1.md"
        ));
    }

    #[tokio::test]
    async fn bootstrap_fails_if_already_initialized() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let invoker = MockLibrarian::succeeding();
        let result = run(&storage, &invoker, "Requirements text.").await;

        assert!(matches!(result, Err(BootstrapError::AlreadyInitialized)));
        assert!(!invoker.was_invoked());
    }

    #[tokio::test]
    async fn bootstrap_fails_if_changelog_exists() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());

        // Only create CHANGELOG.md, not raw/ or derived/.
        let entry = ChangelogEntry::Bootstrap {
            ts: "2026-01-01T00:00:00Z".to_owned(),
            raw: "REQUIREMENTS_1.md".to_owned(),
        };
        storage.append_changelog(&entry).unwrap();

        let invoker = MockLibrarian::succeeding();
        let result = run(&storage, &invoker, "Requirements.").await;
        assert!(matches!(result, Err(BootstrapError::AlreadyInitialized)));
    }

    #[tokio::test]
    async fn bootstrap_librarian_failure_leaves_raw_on_disk() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        let invoker = MockLibrarian::failing();

        let result = run(&storage, &invoker, "Requirements for failing test.").await;
        assert!(matches!(result, Err(BootstrapError::LibrarianFailed(_))));

        // Raw document persists (no rollback).
        let raw_content = storage.read_raw("REQUIREMENTS_1.md").unwrap();
        assert_eq!(raw_content, "Requirements for failing test.");

        // Changelog was NOT written (operation incomplete).
        let entries = storage.read_changelog().unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn bootstrap_creates_expected_directory_structure() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        let invoker = MockLibrarian::succeeding();

        run(&storage, &invoker, "Test requirements.").await.unwrap();

        assert!(storage.raw_dir().is_dir());
        assert!(storage.derived_dir().is_dir());
        assert!(storage.changelog_path().is_file());
    }

    #[tokio::test]
    async fn bootstrap_validation_warnings_do_not_fail_operation() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());

        let warnings = run(&storage, &BadNameLibrarian, "Reqs.").await.unwrap();
        // Operation succeeds despite validation warnings, but warnings are produced.
        assert!(
            !warnings.is_empty(),
            "expected validation warnings for bad filename"
        );

        // Changelog still written.
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
    }
}
