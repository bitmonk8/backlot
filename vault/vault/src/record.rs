// Record operation: writes new content into vault and integrates it via librarian.
//
// Stores raw content at the next version number, invokes the librarian to
// update derived documents, then records the operation in the changelog.

use crate::librarian::LibrarianInvoker;
use crate::prompts;
use crate::storage::{
    ChangelogEntry, DerivedValidationWarning, DocumentRef, Storage, compute_changed,
    utc_now_iso8601,
};
use crate::{RecordError, RecordMode};

/// Execute the record operation.
///
/// Writes content to `raw/NAME_N.md`, invokes the librarian to integrate the
/// new content into derived documents, and appends a changelog entry. Returns
/// references to derived documents that were created or modified.
pub async fn run<L: LibrarianInvoker>(
    storage: &Storage,
    invoker: &L,
    name: &str,
    content: &str,
    mode: RecordMode,
) -> Result<(Vec<DocumentRef>, Vec<DerivedValidationWarning>), RecordError> {
    let new_series = mode == RecordMode::New;

    // Step 1: Write raw document (validates name and version constraints).
    let raw_filename = storage.write_raw_versioned(name, content, new_series)?;

    // Step 2: Snapshot derived documents before librarian invocation.
    let before = storage.snapshot_derived()?;

    // Step 3: Build prompt and invoke librarian.
    let system_prompt = prompts::build_record_prompt(storage, &raw_filename)?;
    let query = prompts::record_query();

    invoker
        .produce_derived(&system_prompt, query, storage)
        .await
        .map_err(RecordError::LibrarianFailed)?;

    // Step 4: Post-invocation validation (warnings only, returned to caller).
    let warnings = storage.validate_derived()?;

    // Step 5: Snapshot after and compute changed set.
    let after = storage.snapshot_derived()?;
    let derived_modified = compute_changed(&before, &after);

    // Step 6: Append changelog entry.
    let entry = ChangelogEntry::Record {
        ts: utc_now_iso8601(),
        raw: raw_filename,
        derived_modified: derived_modified
            .iter()
            .map(|d| d.filename.clone())
            .collect(),
    };
    storage.append_changelog(&entry)?;

    Ok((derived_modified, warnings))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::prompts;
    use crate::storage::Storage;
    use crate::test_support::{
        BadNameLibrarian, CapturingLibrarian, DerivedWriter, MockLibrarian, NoOpLibrarian,
    };
    use std::fs;
    use tempfile::TempDir;

    /// Writer that modifies PROJECT.md and creates FINDINGS.md.
    fn record_writer() -> DerivedWriter {
        Box::new(|storage: &Storage| {
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project Overview\n<!-- scope: top-level project index -->\n\nUpdated with new findings.\n",
            )
            .map_err(|e| e.to_string())?;
            fs::write(
                derived.join("FINDINGS.md"),
                "# Findings\n<!-- scope: research findings -->\n\nFindings content.\n",
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    /// Helper: set up a bootstrapped vault (raw/ + derived/ + PROJECT.md).
    fn setup_bootstrapped(tmp: &TempDir) -> Storage {
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();
        fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project Overview\n<!-- scope: top-level project index -->\n\nOriginal content.\n",
        )
        .unwrap();
        storage
    }

    // -- Test 1: Successful record with RecordMode::New --

    #[tokio::test]
    async fn record_new_succeeds() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = MockLibrarian::succeeding(record_writer());

        let result = run(
            &storage,
            &invoker,
            "FINDINGS",
            "New findings.",
            RecordMode::New,
        )
        .await;
        assert!(result.is_ok());
        assert!(invoker.was_invoked());

        let (modified, _warnings) = result.unwrap();
        // MockLibrarian creates FINDINGS.md and modifies PROJECT.md
        assert!(modified.iter().any(|d| d.filename == "FINDINGS.md"));
        assert!(modified.iter().any(|d| d.filename == "PROJECT.md"));

        // Raw document written
        let raw_content = storage.read_raw("FINDINGS_1.md").unwrap();
        assert_eq!(raw_content, "New findings.");

        // Changelog entry recorded
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ChangelogEntry::Record { raw, derived_modified, .. }
                if raw == "FINDINGS_1.md" && derived_modified.contains(&"FINDINGS.md".to_owned())
        ));
    }

    // -- Test 2: Successful record with RecordMode::Append --

    #[tokio::test]
    async fn record_append_succeeds() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        // Create initial version
        storage
            .write_raw_versioned("FINDINGS", "v1 content", true)
            .unwrap();

        let invoker = MockLibrarian::succeeding(record_writer());
        let result = run(
            &storage,
            &invoker,
            "FINDINGS",
            "v2 content",
            RecordMode::Append,
        )
        .await;
        assert!(result.is_ok());

        // Version 2 created
        let raw_content = storage.read_raw("FINDINGS_2.md").unwrap();
        assert_eq!(raw_content, "v2 content");
    }

    // -- Test 3: RecordMode::New when versions already exist --

    #[tokio::test]
    async fn record_new_version_conflict() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        storage
            .write_raw_versioned("FINDINGS", "existing", true)
            .unwrap();

        let invoker = MockLibrarian::succeeding(record_writer());
        let result = run(
            &storage,
            &invoker,
            "FINDINGS",
            "new content",
            RecordMode::New,
        )
        .await;
        assert!(matches!(result, Err(RecordError::VersionConflict(_))));
        assert!(!invoker.was_invoked());
    }

    // -- Test 4: RecordMode::Append when no versions exist --

    #[tokio::test]
    async fn record_append_not_found() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let invoker = MockLibrarian::succeeding(record_writer());
        let result = run(
            &storage,
            &invoker,
            "FINDINGS",
            "content",
            RecordMode::Append,
        )
        .await;
        assert!(matches!(result, Err(RecordError::DocumentNotFound(_))));
        assert!(!invoker.was_invoked());
    }

    // -- Test 5: Invalid name --

    #[tokio::test]
    async fn record_invalid_name() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let invoker = MockLibrarian::succeeding(record_writer());
        let result = run(&storage, &invoker, "findings", "content", RecordMode::New).await;
        assert!(matches!(result, Err(RecordError::InvalidName(_))));
        assert!(!invoker.was_invoked());
    }

    // -- Test 6: Name with version suffix --

    #[tokio::test]
    async fn record_name_with_version_suffix() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let invoker = MockLibrarian::succeeding(record_writer());
        let result = run(&storage, &invoker, "FINDINGS_2", "content", RecordMode::New).await;
        assert!(matches!(result, Err(RecordError::InvalidName(_))));
        assert!(!invoker.was_invoked());
    }

    // -- Test 7: Librarian failure --

    #[tokio::test]
    async fn record_librarian_failure_leaves_raw_no_changelog() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = MockLibrarian::failing();

        let result = run(&storage, &invoker, "FINDINGS", "content", RecordMode::New).await;
        assert!(matches!(result, Err(RecordError::LibrarianFailed(_))));

        // Raw document persists (no rollback)
        let raw_content = storage.read_raw("FINDINGS_1.md").unwrap();
        assert_eq!(raw_content, "content");

        // Changelog NOT written
        let entries = storage.read_changelog().unwrap();
        assert!(entries.is_empty());
    }

    // -- Test 8: Modified derived documents correctly detected --

    #[tokio::test]
    async fn record_detects_modified_derived() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        // Also create REQUIREMENTS.md that will NOT be modified
        fs::write(
            storage.derived_dir().join("REQUIREMENTS.md"),
            "# Requirements\n<!-- scope: reqs -->\n\nOriginal.\n",
        )
        .unwrap();

        let invoker = MockLibrarian::succeeding(record_writer());
        let (modified, _warnings) = run(&storage, &invoker, "FINDINGS", "content", RecordMode::New)
            .await
            .unwrap();

        // FINDINGS.md created, PROJECT.md modified, REQUIREMENTS.md unchanged
        let names: Vec<&str> = modified.iter().map(|d| d.filename.as_str()).collect();
        assert!(names.contains(&"FINDINGS.md"));
        assert!(names.contains(&"PROJECT.md"));
        assert!(!names.contains(&"REQUIREMENTS.md"));
    }

    // -- Test 9: No derived changes returns empty vec --

    #[tokio::test]
    async fn record_no_derived_changes() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let (modified, _warnings) = run(
            &storage,
            &NoOpLibrarian,
            "FINDINGS",
            "content",
            RecordMode::New,
        )
        .await
        .unwrap();
        assert!(modified.is_empty());

        // Changelog still written with empty derived_modified
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ChangelogEntry::Record { derived_modified, .. } if derived_modified.is_empty()
        ));
    }

    // -- Test 10: Prompt contains required sections --

    #[tokio::test]
    async fn record_prompt_contains_required_sections() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        storage
            .write_raw_versioned("FINDINGS", "v1 content", true)
            .unwrap();

        let invoker = CapturingLibrarian::new(None);
        run(&storage, &invoker, "NOTES", "note content", RecordMode::New)
            .await
            .unwrap();

        let prompt = invoker.captured_prompt.lock().unwrap().clone().unwrap();
        let query = invoker.captured_query.lock().unwrap().clone().unwrap();

        // Shared blocks
        assert!(prompt.contains("Core Principle"), "missing Core Principle");
        assert!(
            prompt.contains("Document Format"),
            "missing Document Format"
        );
        assert!(
            prompt.contains("Cross-References"),
            "missing Cross-References"
        );
        assert!(
            prompt.contains("Scope Restriction"),
            "missing Scope Restriction"
        );
        assert!(
            prompt.contains("Current Document Inventory"),
            "missing inventory"
        );

        // Record-specific block
        assert!(prompt.contains("Record Task"), "missing Record Task");
        assert!(
            prompt.contains("raw/NOTES_1.md"),
            "missing raw document reference in prompt"
        );
        assert!(
            prompt.contains("derived/PROJECT.md"),
            "prompt should mention PROJECT.md for orientation"
        );

        // Query
        assert_eq!(query, prompts::record_query());
    }

    // -- Test 11: Validation warnings do not fail the operation --
    // (BadNameLibrarian creates a bad filename; record still succeeds with warnings)

    #[tokio::test]
    async fn record_validation_warnings_do_not_fail() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        // BadNameLibrarian creates bad_name.md alongside PROJECT.md
        let result = run(
            &storage,
            &BadNameLibrarian,
            "FINDINGS",
            "content",
            RecordMode::New,
        )
        .await;
        // Operation succeeds despite validation warnings, but warnings are produced.
        let (modified, warnings) = result.unwrap();
        assert!(
            !warnings.is_empty(),
            "expected validation warnings for bad filename"
        );
        assert!(!modified.is_empty(), "expected modified derived documents");

        // Changelog still written.
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
    }
}
