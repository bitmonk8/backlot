// Librarian: reel agent invocation for vault operations.
//
// Defines the LibrarianInvoker trait (abstraction over agent calls, enabling
// mock substitution in tests) and the ReelLibrarian production implementation.
// Prompt composition lives in the sibling `prompts` module.

use crate::storage::Storage;

use std::time::Duration;

pub const AGENT_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Agent invocation trait
// ---------------------------------------------------------------------------

/// Trait abstracting librarian invocation so tests can substitute a mock.
///
/// Implementations receive a system prompt, a user query, and a storage handle.
/// The librarian reads raw documents and writes derived documents via the
/// storage root's filesystem — `storage` is provided so implementations can
/// locate `derived/`.
pub trait LibrarianInvoker: Send + Sync {
    fn produce_derived(
        &self,
        system_prompt: &str,
        query: &str,
        storage: &Storage,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

// ---------------------------------------------------------------------------
// Production implementation
// ---------------------------------------------------------------------------

/// Production invoker that delegates to a shared reel `Agent`.
pub struct ReelLibrarian<'a> {
    pub agent: &'a reel::Agent,
    pub model_name: &'a str,
}

impl LibrarianInvoker for ReelLibrarian<'_> {
    async fn produce_derived(
        &self,
        system_prompt: &str,
        query: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        let config = reel::RequestConfig::builder()
            .model(self.model_name)
            .system_prompt(system_prompt)
            .build()
            .map_err(|e| format!("failed to build request config: {e}"))?;

        // Read-only root; write_paths scopes Write/Edit tools to derived/.
        let tool_grants = reel::ToolGrant::TOOLS;

        let request = reel::AgentRequestConfig {
            config,
            grant: tool_grants.normalize(),
            custom_tools: Vec::new(),
            write_paths: vec![storage.derived_dir()],
        };

        let _result: reel::RunResult<String> = self
            .agent
            .run(&request, query)
            .await
            .map_err(|e| format!("librarian agent failed: {e}"))?;

        Ok(())
    }
}
