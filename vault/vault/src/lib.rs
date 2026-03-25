// Vault: library crate.
//
// Persistent, file-based knowledge store for agent systems. Provides the Vault
// struct as the public API entry point. Implements bootstrap and record
// operations, with query and reorganize planned.

pub(crate) mod bootstrap;
pub(crate) mod librarian;
pub(crate) mod prompts;
pub(crate) mod record;
pub(crate) mod storage;
#[cfg(test)]
pub(crate) mod test_support;

use std::path::PathBuf;

use librarian::ReelLibrarian;
use storage::Storage;

pub use storage::DocumentRef;

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

    #[error("storage error: {0}")]
    Storage(String),

    #[error("librarian failed: {0}")]
    LibrarianFailed(String),
}

impl From<storage::StorageError> for BootstrapError {
    fn from(e: storage::StorageError) -> Self {
        Self::Storage(e.to_string())
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
    /// prior state.
    pub async fn bootstrap(&self, requirements: &str) -> Result<(), BootstrapError> {
        let invoker = ReelLibrarian {
            agent: &self.agent,
            model_name: &self.models.bootstrap,
        };

        let warnings = bootstrap::run(&self.storage, &invoker, requirements).await?;
        for w in &warnings {
            eprintln!(
                "vault: bootstrap validation warning: {}: {}",
                w.filename, w.reason
            );
        }
        Ok(())
    }

    /// Record new content into the vault.
    ///
    /// Writes content to `raw/NAME_N.md`, invokes the librarian to integrate
    /// it into derived documents, and records the operation in `CHANGELOG.md`.
    /// Returns references to derived documents that were created or modified.
    pub async fn record(
        &self,
        name: &str,
        content: &str,
        mode: RecordMode,
    ) -> Result<Vec<DocumentRef>, RecordError> {
        let invoker = ReelLibrarian {
            agent: &self.agent,
            model_name: &self.models.record,
        };

        let (modified, warnings) =
            record::run(&self.storage, &invoker, name, content, mode).await?;
        for w in &warnings {
            eprintln!(
                "vault: record validation warning: {}: {}",
                w.filename, w.reason
            );
        }
        Ok(modified)
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
