// Reorganize operation: full restructuring pass over derived documents.
//
// Reviews all derived documents for merging opportunities, structural
// improvements, and deduplication. Unlike the lightweight librarian pass
// on each record, this is a full sweep.

use crate::ReorganizeError;
use crate::librarian::DerivedProducer;
use crate::prompts;
use crate::storage::{
    ChangelogEntry, DerivedValidationWarning, DocumentRef, Storage, compute_changed,
    compute_deleted, utc_now_iso8601,
};

/// Result of a reorganize operation: the report and any validation warnings.
pub type ReorganizeResult = (crate::ReorganizeReport, Vec<DerivedValidationWarning>);

/// Execute the reorganize operation.
///
/// Snapshots derived documents, invokes the librarian for a full restructuring
/// pass, then compares snapshots to detect merged, restructured, and deleted
/// documents. Appends a changelog entry and returns a report.
pub async fn run<L: DerivedProducer>(
    storage: &Storage,
    invoker: &L,
) -> Result<ReorganizeResult, ReorganizeError> {
    // Step 1: Snapshot derived documents before librarian invocation.
    let before = storage.snapshot_derived()?;

    // Step 2: Build prompt and invoke librarian.
    let system_prompt = prompts::build_reorganize_prompt(storage)?;
    let user_message = prompts::reorganize_user_message();

    invoker
        .produce_derived(&system_prompt, user_message, storage)
        .await
        .map_err(ReorganizeError::LibrarianFailed)?;

    // Step 3: Post-invocation validation (warnings only, returned to caller).
    let warnings = storage.validate_derived()?;

    // Step 4: Snapshot after and compute changes.
    let after = storage.snapshot_derived()?;

    // Categorize changes per spec's ReorganizeReport fields:
    // - merged: existing files whose content changed (consolidation/updates)
    // - restructured: new files (created from splits)
    // - deleted: files removed (absorbed into other documents)
    let changed = compute_changed(&before, &after);
    let (merged, restructured): (Vec<DocumentRef>, Vec<DocumentRef>) = changed
        .into_iter()
        .partition(|d| before.contains_key(&d.filename));

    let deleted = compute_deleted(&before, &after);

    // Step 5: Append changelog entry.
    let entry = ChangelogEntry::Reorganize {
        ts: utc_now_iso8601(),
        merged: merged.iter().map(|d| d.filename.clone()).collect(),
        restructured: restructured.iter().map(|d| d.filename.clone()).collect(),
        deleted: deleted.iter().map(|d| d.filename.clone()).collect(),
    };
    storage.append_changelog(&entry)?;

    let report = crate::ReorganizeReport {
        merged,
        restructured,
        deleted,
    };
    Ok((report, warnings))
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
        BadNameLibrarian, CapturingLibrarian, DeletingLibrarian, DerivedWriter, MockLibrarian,
        NoOpLibrarian,
    };
    use std::fs;
    use tempfile::TempDir;

    /// Helper: set up a bootstrapped vault with PROJECT.md and REQUIREMENTS.md.
    fn setup_bootstrapped(tmp: &TempDir) -> Storage {
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();
        fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project Overview\n<!-- scope: top-level project index -->\n\nOriginal content.\n",
        )
        .unwrap();
        fs::write(
            storage.derived_dir().join("REQUIREMENTS.md"),
            "# Requirements\n<!-- scope: structured requirements -->\n\nRequirements content.\n",
        )
        .unwrap();
        storage
    }

    /// Writer that modifies PROJECT.md content (simulates a merge).
    fn merge_writer() -> DerivedWriter {
        Box::new(|storage: &Storage| {
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project Overview\n<!-- scope: top-level project index -->\n\nConsolidated content.\n",
            )
            .map_err(|e| e.to_string())
        })
    }

    /// Writer that creates a new file (simulates a split/restructure).
    fn restructure_writer() -> DerivedWriter {
        Box::new(|storage: &Storage| {
            let derived = storage.derived_dir();
            fs::write(
                derived.join("ARCHITECTURE.md"),
                "# Architecture\n<!-- scope: system architecture -->\n\nArchitecture content.\n",
            )
            .map_err(|e| e.to_string())
        })
    }

    // -- Test 1: Reorganize with no changes --

    #[tokio::test]
    async fn reorganize_no_changes() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let (report, _warnings) = run(&storage, &NoOpLibrarian).await.unwrap();
        assert!(report.merged.is_empty());
        assert!(report.restructured.is_empty());
        assert!(report.deleted.is_empty());

        // Changelog still written.
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], ChangelogEntry::Reorganize { .. }));
    }

    // -- Test 2: Reorganize detects merged (modified) documents --

    #[tokio::test]
    async fn reorganize_detects_merged() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = MockLibrarian::succeeding(merge_writer());

        let (report, _warnings) = run(&storage, &invoker).await.unwrap();
        assert!(report.merged.iter().any(|d| d.filename == "PROJECT.md"));
        assert!(report.restructured.is_empty());
        assert!(report.deleted.is_empty());
    }

    // -- Test 3: Reorganize detects restructured (new) documents --

    #[tokio::test]
    async fn reorganize_detects_restructured() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = MockLibrarian::succeeding(restructure_writer());

        let (report, _warnings) = run(&storage, &invoker).await.unwrap();
        assert!(
            report
                .restructured
                .iter()
                .any(|d| d.filename == "ARCHITECTURE.md")
        );
        assert!(report.merged.is_empty());
        assert!(report.deleted.is_empty());
    }

    // -- Test 4: Reorganize detects deleted documents --

    #[tokio::test]
    async fn reorganize_detects_deleted() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = DeletingLibrarian {
            filename_to_delete: "REQUIREMENTS.md".to_owned(),
        };

        let (report, _warnings) = run(&storage, &invoker).await.unwrap();
        assert!(
            report
                .deleted
                .iter()
                .any(|d| d.filename == "REQUIREMENTS.md")
        );
        assert!(report.merged.is_empty());
        assert!(report.restructured.is_empty());
    }

    // -- Test 5: Reorganize with mixed changes --

    #[tokio::test]
    async fn reorganize_mixed_changes() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        // Writer that: modifies PROJECT.md, creates ARCHITECTURE.md, deletes REQUIREMENTS.md
        let invoker = MockLibrarian::succeeding(Box::new(|storage: &Storage| {
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project Overview\n<!-- scope: top-level project index -->\n\nUpdated.\n",
            )
            .map_err(|e| e.to_string())?;
            fs::write(
                derived.join("ARCHITECTURE.md"),
                "# Architecture\n<!-- scope: architecture -->\n\nNew.\n",
            )
            .map_err(|e| e.to_string())?;
            fs::remove_file(derived.join("REQUIREMENTS.md")).map_err(|e| e.to_string())?;
            Ok(())
        }));

        let (report, _warnings) = run(&storage, &invoker).await.unwrap();
        assert!(report.merged.iter().any(|d| d.filename == "PROJECT.md"));
        assert!(
            report
                .restructured
                .iter()
                .any(|d| d.filename == "ARCHITECTURE.md")
        );
        assert!(
            report
                .deleted
                .iter()
                .any(|d| d.filename == "REQUIREMENTS.md")
        );
    }

    // -- Test 6: Librarian failure --

    #[tokio::test]
    async fn reorganize_librarian_failure() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = MockLibrarian::failing();

        let result = run(&storage, &invoker).await;
        assert!(matches!(result, Err(ReorganizeError::LibrarianFailed(_))));

        // Changelog NOT written.
        let entries = storage.read_changelog().unwrap();
        assert!(entries.is_empty());
    }

    // -- Test 7: Prompt contains required sections --

    #[tokio::test]
    async fn reorganize_prompt_contains_required_sections() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);
        let invoker = CapturingLibrarian::new(None);

        run(&storage, &invoker).await.unwrap();

        let prompt = invoker.captured_prompt.lock().unwrap().clone().unwrap();
        let message = invoker.captured_message.lock().unwrap().clone().unwrap();

        // Shared blocks
        assert!(prompt.contains("Core Principle"), "missing Core Principle");
        assert!(
            prompt.contains("Scope Restriction"),
            "missing Scope Restriction"
        );
        assert!(
            prompt.contains("Current Document Inventory"),
            "missing inventory"
        );

        // Reorganize-specific block
        assert!(
            prompt.contains("Reorganize Task"),
            "missing Reorganize Task"
        );
        assert!(prompt.contains("lifecycle triggers"));

        // User message
        assert_eq!(message, prompts::reorganize_user_message());
    }

    // -- Test 8: Validation warnings do not fail the operation --

    #[tokio::test]
    async fn reorganize_validation_warnings_do_not_fail() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let (_, warnings) = run(&storage, &BadNameLibrarian).await.unwrap();
        assert!(
            !warnings.is_empty(),
            "expected validation warnings for bad filename"
        );

        // Changelog still written.
        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
    }

    // -- Test 9: Changelog entry format --

    #[tokio::test]
    async fn reorganize_changelog_entry_format() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let invoker = MockLibrarian::succeeding(Box::new(|storage: &Storage| {
            let derived = storage.derived_dir();
            fs::write(
                derived.join("PROJECT.md"),
                "# Project Overview\n<!-- scope: top-level -->\n\nUpdated.\n",
            )
            .map_err(|e| e.to_string())?;
            fs::remove_file(derived.join("REQUIREMENTS.md")).map_err(|e| e.to_string())?;
            Ok(())
        }));

        run(&storage, &invoker).await.unwrap();

        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ChangelogEntry::Reorganize {
                merged,
                restructured,
                deleted,
                ..
            } => {
                assert!(merged.contains(&"PROJECT.md".to_owned()));
                assert!(restructured.is_empty());
                assert!(deleted.contains(&"REQUIREMENTS.md".to_owned()));
            }
            other => panic!("expected Reorganize entry, got {other:?}"),
        }
    }
}
