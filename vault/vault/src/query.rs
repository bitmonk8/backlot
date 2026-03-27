// Query operation: answers questions from the vault's knowledge base.
//
// Read-only operation. The librarian reads raw and derived documents to
// synthesize an answer. No files are written, no changelog entry is appended.

use crate::SessionMetadata;
use crate::librarian::QueryResponder;
use crate::prompts;
use crate::storage::Storage;
use crate::{QueryError, QueryResult};

/// Query result: the structured answer and session metadata.
pub type QueryOpResult = (QueryResult, SessionMetadata);

/// Execute the query operation.
///
/// Builds a query-specific system prompt, invokes the librarian to read
/// documents and synthesize an answer, and returns the structured result
/// with session metadata. No files are written and no changelog entry is
/// appended.
pub async fn run<L: QueryResponder>(
    storage: &Storage,
    invoker: &L,
    question: &str,
) -> Result<QueryOpResult, QueryError> {
    let system_prompt = prompts::build_query_prompt(storage)
        .map_err(|e| QueryError::Io(std::io::Error::other(e.to_string())))?;
    let user_message = prompts::query_user_message(question);

    invoker
        .answer_query(&system_prompt, &user_message, storage)
        .await
        .map_err(QueryError::LibrarianFailed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::{DocumentRef, Storage};
    use crate::test_support::{CapturingLibrarian, MockQueryLibrarian};
    use crate::{Coverage, Extract, QueryResult};
    use std::fs;
    use tempfile::TempDir;

    /// Helper: set up a bootstrapped vault with PROJECT.md and a raw document.
    fn setup_bootstrapped(tmp: &TempDir) -> Storage {
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();
        fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project Overview\n<!-- scope: top-level project index -->\n\nProject content.\n",
        )
        .unwrap();
        storage
    }

    #[tokio::test]
    async fn query_full_coverage() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let result = QueryResult {
            coverage: Coverage::Full,
            answer: "The project is a widget system.".to_owned(),
            extracts: vec![Extract {
                content: "Widget system overview".to_owned(),
                source: DocumentRef {
                    filename: "PROJECT.md".to_owned(),
                },
            }],
        };
        let invoker = MockQueryLibrarian::succeeding(result);

        let (qr, _metadata) = run(&storage, &invoker, "What is the project?")
            .await
            .unwrap();
        assert!(matches!(qr.coverage, Coverage::Full));
        assert_eq!(qr.answer, "The project is a widget system.");
        assert_eq!(qr.extracts.len(), 1);
        assert_eq!(qr.extracts[0].source.filename, "PROJECT.md");
    }

    #[tokio::test]
    async fn query_partial_coverage() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let result = QueryResult {
            coverage: Coverage::Partial,
            answer: "Some info found but incomplete.".to_owned(),
            extracts: vec![],
        };
        let invoker = MockQueryLibrarian::succeeding(result);

        let (qr, _metadata) = run(&storage, &invoker, "Tell me about deployment.")
            .await
            .unwrap();
        assert!(matches!(qr.coverage, Coverage::Partial));
    }

    #[tokio::test]
    async fn query_none_coverage() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let result = QueryResult {
            coverage: Coverage::None,
            answer: "No relevant information found.".to_owned(),
            extracts: vec![],
        };
        let invoker = MockQueryLibrarian::succeeding(result);

        let (qr, _metadata) = run(&storage, &invoker, "What is the weather?")
            .await
            .unwrap();
        assert!(matches!(qr.coverage, Coverage::None));
        assert!(qr.extracts.is_empty());
    }

    #[tokio::test]
    async fn query_multiple_extracts() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        // Add another derived document
        fs::write(
            storage.derived_dir().join("REQUIREMENTS.md"),
            "# Requirements\n<!-- scope: reqs -->\n\nRequirements content.\n",
        )
        .unwrap();

        let result = QueryResult {
            coverage: Coverage::Full,
            answer: "Architecture uses layers.".to_owned(),
            extracts: vec![
                Extract {
                    content: "Layer diagram from project".to_owned(),
                    source: DocumentRef {
                        filename: "PROJECT.md".to_owned(),
                    },
                },
                Extract {
                    content: "Requirement for layered design".to_owned(),
                    source: DocumentRef {
                        filename: "REQUIREMENTS.md".to_owned(),
                    },
                },
            ],
        };
        let invoker = MockQueryLibrarian::succeeding(result);

        let (qr, _metadata) = run(&storage, &invoker, "How is the architecture organized?")
            .await
            .unwrap();
        assert_eq!(qr.extracts.len(), 2);
        assert_eq!(qr.extracts[0].source.filename, "PROJECT.md");
        assert_eq!(qr.extracts[1].source.filename, "REQUIREMENTS.md");
    }

    #[tokio::test]
    async fn query_librarian_failure() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let invoker = MockQueryLibrarian::failing();

        let err = run(&storage, &invoker, "A question.").await.unwrap_err();
        assert!(matches!(err, QueryError::LibrarianFailed(_)));
    }

    #[tokio::test]
    async fn query_passes_correct_prompt_and_message() {
        let tmp = TempDir::new().unwrap();
        let storage = setup_bootstrapped(&tmp);

        let invoker = CapturingLibrarian::new(Some(Box::new(|_| Ok(()))));

        run(&storage, &invoker, "What does the project do?")
            .await
            .unwrap();

        let prompt = invoker.captured_prompt.lock().unwrap().clone().unwrap();
        let message = invoker.captured_message.lock().unwrap().clone().unwrap();

        assert!(prompt.contains("Query Task"), "missing Query Task block");
        assert!(
            prompt.contains("must NOT write"),
            "missing read-only constraint"
        );

        assert!(
            message.contains("What does the project do?"),
            "user message should contain the question"
        );
    }
}
