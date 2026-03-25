// Shared test utilities for vault operation tests.
//
// Provides reusable mock LibrarianInvoker implementations and helpers,
// eliminating duplication between bootstrap and record test modules.

use crate::librarian::LibrarianInvoker;
use crate::storage::Storage;

use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Type alias for a function that writes files to the derived directory.
pub type DerivedWriter = Box<dyn Fn(&Storage) -> Result<(), String> + Send + Sync>;

/// Mock librarian with configurable success/failure and derived file output.
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

        (self.writer)(storage)
    }
}

/// Mock librarian that does nothing (no derived changes).
pub struct NoOpLibrarian;

impl LibrarianInvoker for NoOpLibrarian {
    async fn produce_derived(
        &self,
        _system_prompt: &str,
        _query: &str,
        _storage: &Storage,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Mock librarian that captures the system prompt and query for verification.
/// Optionally writes derived files via a configurable writer.
pub struct CapturingLibrarian {
    pub captured_prompt: Mutex<Option<String>>,
    pub captured_query: Mutex<Option<String>>,
    writer: DerivedWriter,
}

impl CapturingLibrarian {
    /// Create with an optional writer. If None, writes a minimal PROJECT.md.
    pub fn new(writer: Option<DerivedWriter>) -> Self {
        Self {
            captured_prompt: Mutex::new(None),
            captured_query: Mutex::new(None),
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

impl LibrarianInvoker for CapturingLibrarian {
    async fn produce_derived(
        &self,
        system_prompt: &str,
        query: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        *self.captured_prompt.lock().map_err(|e| e.to_string())? = Some(system_prompt.to_owned());
        *self.captured_query.lock().map_err(|e| e.to_string())? = Some(query.to_owned());
        (self.writer)(storage)
    }
}

/// Mock librarian that writes a file with a bad filename (for validation warning tests).
pub struct BadNameLibrarian;

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
        fs::write(derived.join("bad_name.md"), "no header\n").map_err(|e| e.to_string())?;
        Ok(())
    }
}
