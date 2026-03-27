// Vault: library crate.
//
// Persistent, file-based knowledge store for agent systems. Provides the Vault
// struct as the public API entry point. Implements bootstrap, record, query,
// and reorganize operations.

pub(crate) mod bootstrap;
pub(crate) mod librarian;
pub(crate) mod prompts;
pub(crate) mod query;
pub(crate) mod record;
pub(crate) mod reorganize;
pub(crate) mod storage;
#[cfg(test)]
pub(crate) mod test_support;

use std::path::PathBuf;

use librarian::ReelLibrarian;
use storage::Storage;

pub use storage::{DerivedValidationWarning, DocumentRef};

// ---------------------------------------------------------------------------
// Session metadata (observability)
// ---------------------------------------------------------------------------

/// Per-turn record from an agent session transcript.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptTurn {
    /// Tool calls the model requested in this turn.
    pub tool_calls: Vec<TranscriptToolCall>,
    /// Token usage for this model call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnUsage>,
    /// API latency in milliseconds for this model call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_latency_ms: Option<u64>,
}

/// A single tool call within a transcript turn.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptToolCall {
    pub tool_use_id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// Token usage for a single turn.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub cache_creation_input_tokens: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
}

#[allow(clippy::trivially_copy_pass_by_ref, clippy::missing_const_for_fn)]
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

/// Aggregated session metadata from a librarian invocation.
///
/// Extracted from reel's `RunResult` to decouple vault's public API from reel
/// internals. Includes token usage, tool call count, and the full session
/// transcript.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionMetadata {
    /// Total input tokens across the session.
    pub input_tokens: u64,
    /// Total output tokens across the session.
    pub output_tokens: u64,
    /// Tokens used to create prompt cache entries (zero when caching inactive).
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub cache_creation_input_tokens: u64,
    /// Tokens read from prompt cache (zero when caching inactive).
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub cache_read_input_tokens: u64,
    /// Estimated cost in USD (if the model has pricing info).
    pub cost_usd: f64,
    /// Total number of tool calls executed during the session.
    pub tool_calls: u32,
    /// Session transcript: one entry per model call, in order.
    pub transcript: Vec<TranscriptTurn>,
}

impl SessionMetadata {
    /// Build from a reel `RunResult`.
    pub(crate) fn from_run_result<T>(result: &reel::RunResult<T>) -> Self {
        let (input_tokens, output_tokens, cache_creation, cache_read, cost_usd) =
            match &result.usage {
                Some(u) => (
                    u.input_tokens,
                    u.output_tokens,
                    u.cache_creation_input_tokens,
                    u.cache_read_input_tokens,
                    u.cost_usd,
                ),
                None => (0, 0, 0, 0, 0.0),
            };

        let transcript = result
            .transcript
            .iter()
            .map(|turn| TranscriptTurn {
                tool_calls: turn
                    .tool_calls
                    .iter()
                    .map(|tc| TranscriptToolCall {
                        tool_use_id: tc.tool_use_id.clone(),
                        name: tc.name.clone(),
                        input: tc.input.clone(),
                    })
                    .collect(),
                usage: turn.usage.as_ref().map(|u| TurnUsage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cache_creation_input_tokens: u.cache_creation_input_tokens,
                    cache_read_input_tokens: u.cache_read_input_tokens,
                    cost_usd: u.cost_usd,
                }),
                api_latency_ms: turn.api_latency_ms,
            })
            .collect();

        Self {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
            cost_usd,
            tool_calls: result.tool_calls,
            transcript,
        }
    }

    /// Compute total API latency by summing per-turn latencies.
    pub fn api_latency_ms(&self) -> u64 {
        self.transcript
            .iter()
            .filter_map(|t| t.api_latency_ms)
            .sum()
    }

    /// Create a synthetic empty metadata (for tests).
    #[cfg(test)]
    pub(crate) const fn empty() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
            tool_calls: 0,
            transcript: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Runtime environment for a vault instance.
pub struct VaultEnvironment {
    pub storage_root: PathBuf,
    pub model_registry: reel::ModelRegistry,
    pub provider_registry: reel::ProviderRegistry,
    pub models: VaultModels,
}

/// Model identifiers for each vault operation.
pub struct VaultModels {
    pub bootstrap: String,
    pub query: String,
    pub record: String,
    pub reorganize: String,
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum VaultCreateError {
    #[error("storage root does not exist or is not a directory: {0}")]
    StorageUnavailable(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("bootstrap called on an already-initialized vault")]
    AlreadyInitialized,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("librarian failed: {0}")]
    LibrarianFailed(String),
}

impl From<storage::StorageError> for BootstrapError {
    fn from(e: storage::StorageError) -> Self {
        Self::Io(std::io::Error::other(e.to_string()))
    }
}

/// Whether to create a new document series or append to an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordMode {
    /// Create a new document series. Fails if any version already exists.
    New,
    /// Append a new version to an existing document series.
    Append,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("invalid document name: {0}")]
    InvalidName(String),

    #[error("version conflict: {0}")]
    VersionConflict(String),

    #[error("document not found: {0}")]
    DocumentNotFound(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("librarian failed: {0}")]
    LibrarianFailed(String),
}

impl From<storage::StorageError> for RecordError {
    fn from(e: storage::StorageError) -> Self {
        match e {
            storage::StorageError::InvalidName(s) => Self::InvalidName(s),
            storage::StorageError::VersionConflict(s) => Self::VersionConflict(s),
            storage::StorageError::DocumentNotFound(s) => Self::DocumentNotFound(s),
            storage::StorageError::Io(io_err) => Self::Io(io_err),
        }
    }
}

/// Coverage assessment for a query answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Coverage {
    /// The vault's documents fully address the question.
    Full,
    /// The vault's documents partially address the question.
    Partial,
    /// The vault's documents do not address the question.
    None,
}

/// Structured result from a query operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryResult {
    pub coverage: Coverage,
    pub answer: String,
    pub extracts: Vec<Extract>,
}

/// An extract from a vault document supporting a query answer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Extract {
    pub content: String,
    pub source: DocumentRef,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("librarian failed: {0}")]
    LibrarianFailed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ReorganizeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("librarian failed: {0}")]
    LibrarianFailed(String),
}

impl From<storage::StorageError> for ReorganizeError {
    fn from(e: storage::StorageError) -> Self {
        Self::Io(std::io::Error::other(e.to_string()))
    }
}

/// Report produced by a reorganize operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReorganizeReport {
    pub merged: Vec<DocumentRef>,
    pub restructured: Vec<DocumentRef>,
    pub deleted: Vec<DocumentRef>,
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

/// Main vault handle. Owns configuration and storage access.
///
/// Holds a reel `Agent` created at construction time. The agent is reused
/// across operations; per-call configuration (model, prompt, grants) is
/// passed via `AgentRequestConfig`.
pub struct Vault {
    storage: Storage,
    agent: reel::Agent,
    models: VaultModels,
}

impl Vault {
    /// Create a new vault instance. Fails if the storage root does not exist.
    ///
    /// Consumes the model and provider registries to build a reusable reel
    /// `Agent`. The agent's `project_root` is set to the storage root.
    pub fn new(env: VaultEnvironment) -> Result<Self, VaultCreateError> {
        if !env.storage_root.is_dir() {
            return Err(VaultCreateError::StorageUnavailable(env.storage_root));
        }

        let agent_env = reel::AgentEnvironment {
            model_registry: env.model_registry,
            provider_registry: env.provider_registry,
            project_root: env.storage_root.clone(),
            timeout: librarian::AGENT_TIMEOUT,
        };

        Ok(Self {
            storage: Storage::new(env.storage_root),
            agent: reel::Agent::new(agent_env),
            models: env.models,
        })
    }

    /// Bootstrap the vault from raw requirements text.
    ///
    /// Writes requirements to `raw/REQUIREMENTS_1.md`, invokes the librarian to
    /// produce derived documents in `derived/`, and records the operation in
    /// `CHANGELOG.md`. Fails with `AlreadyInitialized` if the vault has any
    /// prior state. Returns validation warnings and session metadata.
    pub async fn bootstrap(
        &self,
        requirements: &str,
    ) -> Result<(Vec<DerivedValidationWarning>, SessionMetadata), BootstrapError> {
        let invoker = ReelLibrarian {
            agent: &self.agent,
            model_name: &self.models.bootstrap,
        };

        bootstrap::run(&self.storage, &invoker, requirements).await
    }

    /// Record new content into the vault.
    ///
    /// Writes content to `raw/NAME_N.md`, invokes the librarian to integrate
    /// it into derived documents, and records the operation in `CHANGELOG.md`.
    /// Returns references to derived documents that were created or modified,
    /// validation warnings, and session metadata.
    pub async fn record(
        &self,
        name: &str,
        content: &str,
        mode: RecordMode,
    ) -> Result<
        (
            Vec<DocumentRef>,
            Vec<DerivedValidationWarning>,
            SessionMetadata,
        ),
        RecordError,
    > {
        let invoker = ReelLibrarian {
            agent: &self.agent,
            model_name: &self.models.record,
        };

        record::run(&self.storage, &invoker, name, content, mode).await
    }

    /// Query the vault's knowledge base.
    ///
    /// Read-only operation. Invokes the librarian to read documents and
    /// synthesize an answer. No files are written and no changelog entry
    /// is appended. Returns the structured query result and session metadata.
    pub async fn query(
        &self,
        question: &str,
    ) -> Result<(QueryResult, SessionMetadata), QueryError> {
        let invoker = ReelLibrarian {
            agent: &self.agent,
            model_name: &self.models.query,
        };

        query::run(&self.storage, &invoker, question).await
    }

    /// Reorganize the vault's derived documents.
    ///
    /// Full restructuring pass: merges, splits, deduplicates, and tightens
    /// derived documents. Appends a changelog entry. Returns a report of
    /// merged, restructured, and deleted documents, validation warnings, and
    /// session metadata.
    pub async fn reorganize(
        &self,
    ) -> Result<
        (
            ReorganizeReport,
            Vec<DerivedValidationWarning>,
            SessionMetadata,
        ),
        ReorganizeError,
    > {
        let invoker = ReelLibrarian {
            agent: &self.agent,
            model_name: &self.models.reorganize,
        };

        reorganize::run(&self.storage, &invoker).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_models() -> VaultModels {
        VaultModels {
            bootstrap: "test".into(),
            query: "test".into(),
            record: "test".into(),
            reorganize: "test".into(),
        }
    }

    fn test_registries() -> (reel::ModelRegistry, reel::ProviderRegistry) {
        let model_registry =
            reel::ModelRegistry::from_map(std::collections::BTreeMap::new()).unwrap();
        let provider_registry = reel::ProviderRegistry::load_default().unwrap();
        (model_registry, provider_registry)
    }

    #[test]
    fn api_latency_ms_sums_present_values() {
        let metadata = SessionMetadata {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.0,
            tool_calls: 0,
            transcript: vec![
                TranscriptTurn {
                    tool_calls: Vec::new(),
                    usage: None,
                    api_latency_ms: Some(100),
                },
                TranscriptTurn {
                    tool_calls: Vec::new(),
                    usage: None,
                    api_latency_ms: None,
                },
                TranscriptTurn {
                    tool_calls: Vec::new(),
                    usage: None,
                    api_latency_ms: Some(250),
                },
            ],
        };
        assert_eq!(metadata.api_latency_ms(), 350);
    }

    #[test]
    fn api_latency_ms_empty_transcript_returns_zero() {
        let metadata = SessionMetadata::empty();
        assert_eq!(metadata.api_latency_ms(), 0);
    }

    #[test]
    fn new_fails_if_storage_root_does_not_exist() {
        let (model_registry, provider_registry) = test_registries();
        let env = VaultEnvironment {
            storage_root: PathBuf::from("Z:\\nonexistent\\path\\that\\does\\not\\exist"),
            model_registry,
            provider_registry,
            models: test_models(),
        };

        let result = Vault::new(env);
        assert!(matches!(
            result,
            Err(VaultCreateError::StorageUnavailable(_))
        ));
    }

    #[test]
    fn new_fails_if_storage_root_is_a_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let (model_registry, provider_registry) = test_registries();
        let env = VaultEnvironment {
            storage_root: tmp.path().to_path_buf(),
            model_registry,
            provider_registry,
            models: test_models(),
        };

        let result = Vault::new(env);
        assert!(matches!(
            result,
            Err(VaultCreateError::StorageUnavailable(_))
        ));
    }

    #[test]
    fn new_succeeds_with_valid_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (model_registry, provider_registry) = test_registries();
        let env = VaultEnvironment {
            storage_root: tmp.path().to_path_buf(),
            model_registry,
            provider_registry,
            models: test_models(),
        };

        let result = Vault::new(env);
        assert!(result.is_ok());
    }
}
