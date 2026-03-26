// Shared test utilities for vault operation tests.
//
// Provides reusable mock implementations of DerivedProducer and QueryResponder,
// eliminating duplication between operation test modules.

use crate::librarian::{DerivedProducer, QueryResponder};
use crate::storage::Storage;
use crate::{Coverage, QueryResult};

use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Type alias for a function that writes files to the derived directory.
pub type DerivedWriter = Box<dyn Fn(&Storage) -> Result<(), String> + Send + Sync>;

/// Mock librarian with configurable success/failure and derived file output.
/// Used by bootstrap and record tests.
pub struct MockLibrarian {
    succeed: bool,
    invoked: AtomicBool,
    writer: DerivedWriter,
}

impl MockLibrarian {
    /// Create a succeeding mock that uses the given writer to produce derived files.
    pub fn succeeding(writer: DerivedWriter) -> Self {
        Self {
            succeed: true,
            invoked: AtomicBool::new(false),
            writer,
        }
    }

    /// Create a failing mock (writer is never called).
    pub fn failing() -> Self {
        Self {
            succeed: false,
            invoked: AtomicBool::new(false),
            writer: Box::new(|_| Ok(())),
        }
    }

    pub fn was_invoked(&self) -> bool {
        self.invoked.load(Ordering::Relaxed)
    }
}

impl DerivedProducer for MockLibrarian {
    async fn produce_derived(
        &self,
        _system_prompt: &str,
        _user_message: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        self.invoked.store(true, Ordering::Relaxed);

        if !self.succeed {
            return Err("mock librarian failure".to_owned());
        }

        (self.writer)(storage)
    }
}

/// Mock librarian that does nothing (no derived changes).
pub struct NoOpLibrarian;

impl DerivedProducer for NoOpLibrarian {
    async fn produce_derived(
        &self,
        _system_prompt: &str,
        _user_message: &str,
        _storage: &Storage,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Mock librarian that captures the system prompt and user message for
/// verification. Optionally writes derived files via a configurable writer.
/// Implements both traits for use in bootstrap/record and query tests.
pub struct CapturingLibrarian {
    pub captured_prompt: Mutex<Option<String>>,
    pub captured_message: Mutex<Option<String>>,
    writer: DerivedWriter,
}

impl CapturingLibrarian {
    /// Create with an optional writer. If None, writes a minimal PROJECT.md.
    pub fn new(writer: Option<DerivedWriter>) -> Self {
        Self {
            captured_prompt: Mutex::new(None),
            captured_message: Mutex::new(None),
            writer: writer.unwrap_or_else(|| {
                Box::new(|storage: &Storage| {
                    fs::write(
                        storage.derived_dir().join("PROJECT.md"),
                        "# Project\n<!-- scope: overview -->\n",
                    )
                    .map_err(|e| e.to_string())
                })
            }),
        }
    }
}

impl DerivedProducer for CapturingLibrarian {
    async fn produce_derived(
        &self,
        system_prompt: &str,
        user_message: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        *self.captured_prompt.lock().map_err(|e| e.to_string())? = Some(system_prompt.to_owned());
        *self.captured_message.lock().map_err(|e| e.to_string())? = Some(user_message.to_owned());
        (self.writer)(storage)
    }
}

impl QueryResponder for CapturingLibrarian {
    async fn answer_query(
        &self,
        system_prompt: &str,
        user_message: &str,
        _storage: &Storage,
    ) -> Result<QueryResult, String> {
        *self.captured_prompt.lock().map_err(|e| e.to_string())? = Some(system_prompt.to_owned());
        *self.captured_message.lock().map_err(|e| e.to_string())? = Some(user_message.to_owned());
        Ok(QueryResult {
            coverage: Coverage::None,
            answer: String::new(),
            extracts: Vec::new(),
        })
    }
}

/// Mock librarian that writes a file with a bad filename (for validation warning tests).
pub struct BadNameLibrarian;

impl DerivedProducer for BadNameLibrarian {
    async fn produce_derived(
        &self,
        _system_prompt: &str,
        _user_message: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        let derived = storage.derived_dir();
        fs::write(
            derived.join("PROJECT.md"),
            "# Project\n<!-- scope: overview -->\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(derived.join("bad_name.md"), "no header\n").map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Mock librarian that deletes a file from `derived/` (for reorganize delete detection tests).
pub struct DeletingLibrarian {
    /// Filename to delete from `derived/`.
    pub filename_to_delete: String,
}

impl DerivedProducer for DeletingLibrarian {
    async fn produce_derived(
        &self,
        _system_prompt: &str,
        _user_message: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        let path = storage.derived_dir().join(&self.filename_to_delete);
        fs::remove_file(path).map_err(|e| e.to_string())
    }
}

/// Mock librarian for query operations. Returns a predetermined QueryResult
/// or fails.
pub struct MockQueryLibrarian {
    succeed: bool,
    result: Mutex<Option<QueryResult>>,
}

impl MockQueryLibrarian {
    /// Create a succeeding mock that returns the given QueryResult.
    pub fn succeeding(result: QueryResult) -> Self {
        Self {
            succeed: true,
            result: Mutex::new(Some(result)),
        }
    }

    /// Create a failing mock.
    pub fn failing() -> Self {
        Self {
            succeed: false,
            result: Mutex::new(None),
        }
    }
}

impl QueryResponder for MockQueryLibrarian {
    async fn answer_query(
        &self,
        _system_prompt: &str,
        _user_message: &str,
        _storage: &Storage,
    ) -> Result<QueryResult, String> {
        if !self.succeed {
            return Err("mock query librarian failure".to_owned());
        }

        self.result
            .lock()
            .map_err(|e| e.to_string())?
            .take()
            .ok_or_else(|| "MockQueryLibrarian result already consumed".to_owned())
    }
}
