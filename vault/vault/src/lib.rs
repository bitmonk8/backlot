// Vault: library crate.
//
// Persistent, file-based knowledge store for agent systems. Provides the Vault
// struct as the public API entry point, with bootstrap as the first operation.

pub(crate) mod bootstrap;
pub(crate) mod librarian;
pub(crate) mod prompts;
pub(crate) mod storage;

use std::path::PathBuf;

use librarian::ReelLibrarian;
use storage::Storage;

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
