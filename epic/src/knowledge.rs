// Vault integration: ResearchTool (reel ToolHandler) and usage conversion.

use crate::agent::SessionMeta;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// SessionMeta conversion from vault metadata
// ---------------------------------------------------------------------------

impl SessionMeta {
    /// Convert vault `SessionMetadata` to epic's `SessionMeta`.
    pub fn from_vault(m: &vault::SessionMetadata) -> Self {
        Self {
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            cache_creation_input_tokens: m.cache_creation_input_tokens,
            cache_read_input_tokens: m.cache_read_input_tokens,
            cost_usd: m.cost_usd,
            tool_calls: m.tool_calls,
            total_latency_ms: m.api_latency_ms(),
        }
    }
}

// ---------------------------------------------------------------------------
// ResearchTool — reel ToolHandler wrapping vault::Vault::query()
// ---------------------------------------------------------------------------

/// Custom reel tool that exposes vault queries to agents.
///
/// Agents call `ResearchQuery` during implementation and design phases to
/// retrieve accumulated project knowledge, prior discoveries, and design
/// decisions from the vault's derived documents.
pub struct ResearchTool {
    vault: Arc<vault::Vault>,
    usage_sink: Arc<Mutex<Vec<SessionMeta>>>,
}

impl reel::ToolHandler for ResearchTool {
    fn definition(&self) -> reel::ToolDefinition {
        reel::ToolDefinition {
            name: "ResearchQuery".into(),
            description: "Query the project knowledge base for accumulated research, \
                          discoveries, requirements, and design decisions. Use when you \
                          need context about the project or answers to questions about \
                          prior work."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to research against accumulated project knowledge"
                    }
                },
                "required": ["question"]
            }),
        }
    }

    fn execute<'a>(
        &'a self,
        tool_use_id: String,
        input: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = reel::ToolExecResult> + Send + 'a>> {
        Box::pin(async move {
            let question = input
                .get("question")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            if question.is_empty() {
                return reel::ToolExecResult {
                    tool_use_id,
                    content: "Error: question parameter is required and must be non-empty.".into(),
                    is_error: true,
                };
            }

            match self.vault.query(question).await {
                Ok((result, meta)) => {
                    self.usage_sink
                        .lock()
                        .unwrap()
                        .push(SessionMeta::from_vault(&meta));
                    let content = format_query_result(&result);
                    reel::ToolExecResult {
                        tool_use_id,
                        content,
                        is_error: false,
                    }
                }
                Err(e) => reel::ToolExecResult {
                    tool_use_id,
                    content: format!("Research query failed: {e}"),
                    is_error: true,
                },
            }
        })
    }
}

/// Format a vault `QueryResult` as readable text for the agent.
fn format_query_result(result: &vault::QueryResult) -> String {
    let mut out = String::new();

    let coverage = match result.coverage {
        vault::Coverage::Full => "Full",
        vault::Coverage::Partial => "Partial",
        vault::Coverage::None => "None",
    };
    let _ = write!(out, "Coverage: {coverage}\n\n");
    out.push_str(&result.answer);

    if !result.extracts.is_empty() {
        out.push_str("\n\n--- Supporting Extracts ---\n");
        for extract in &result.extracts {
            let _ = write!(
                out,
                "\n[{}]\n{}\n",
                extract.source.filename, extract.content
            );
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Builder helper
// ---------------------------------------------------------------------------

/// Create a `ResearchTool` and its usage sink.
///
/// Returns the boxed tool (for `AgentRequestConfig::custom_tools`) and the
/// sink handle (drain after agent run to fold vault query costs into task usage).
pub fn build_research_tool(
    vault: &Arc<vault::Vault>,
) -> (Box<dyn reel::ToolHandler>, Arc<Mutex<Vec<SessionMeta>>>) {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let tool = ResearchTool {
        vault: vault.clone(),
        usage_sink: sink.clone(),
    };
    (Box::new(tool), sink)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_vault_metadata_maps_fields() {
        let meta = vault::SessionMetadata {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 10,
            cache_read_input_tokens: 20,
            cost_usd: 0.005,
            tool_calls: 3,
            transcript: vec![
                vault::TranscriptTurn {
                    tool_calls: Vec::new(),
                    usage: None,
                    api_latency_ms: Some(100),
                },
                vault::TranscriptTurn {
                    tool_calls: Vec::new(),
                    usage: None,
                    api_latency_ms: Some(200),
                },
            ],
        };

        let session = SessionMeta::from_vault(&meta);
        assert_eq!(session.input_tokens, 100);
        assert_eq!(session.output_tokens, 50);
        assert_eq!(session.cache_creation_input_tokens, 10);
        assert_eq!(session.cache_read_input_tokens, 20);
        assert!((session.cost_usd - 0.005).abs() < f64::EPSILON);
        assert_eq!(session.tool_calls, 3);
        assert_eq!(session.total_latency_ms, 300);
    }

    #[test]
    fn format_query_result_full_coverage() {
        let result = vault::QueryResult {
            coverage: vault::Coverage::Full,
            answer: "The system uses DFS traversal.".into(),
            extracts: vec![vault::Extract {
                content: "DFS is used for task execution.".into(),
                source: vault::DocumentRef {
                    filename: "DESIGN.md".into(),
                },
            }],
        };
        let formatted = format_query_result(&result);
        assert!(formatted.contains("Coverage: Full"));
        assert!(formatted.contains("The system uses DFS traversal."));
        assert!(formatted.contains("[DESIGN.md]"));
        assert!(formatted.contains("DFS is used for task execution."));
    }

    #[test]
    fn format_query_result_no_extracts() {
        let result = vault::QueryResult {
            coverage: vault::Coverage::None,
            answer: "No information available.".into(),
            extracts: vec![],
        };
        let formatted = format_query_result(&result);
        assert!(formatted.contains("Coverage: None"));
        assert!(!formatted.contains("Supporting Extracts"));
    }

    #[test]
    fn research_tool_definition_schema() {
        let vault = make_dummy_vault();
        let (tool, _sink) = build_research_tool(&vault);
        let def = tool.definition();
        assert_eq!(def.name, "ResearchQuery");
        let props = def.parameters.get("properties").unwrap();
        assert!(props.get("question").is_some());
        let required = def.parameters.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("question")));
    }

    fn make_dummy_vault() -> Arc<vault::Vault> {
        let tmp = tempfile::TempDir::new().unwrap();
        let model_registry =
            reel::ModelRegistry::from_map(std::collections::BTreeMap::new()).unwrap();
        let provider_registry = reel::ProviderRegistry::load_default().unwrap();
        let env = vault::VaultEnvironment {
            storage_root: tmp.path().to_path_buf(),
            model_registry,
            provider_registry,
            models: vault::VaultModels {
                bootstrap: "test".into(),
                query: "test".into(),
                record: "test".into(),
                reorganize: "test".into(),
            },
        };
        // Leak the TempDir so it stays alive for the duration of the test.
        std::mem::forget(tmp);
        Arc::new(vault::Vault::new(env).unwrap())
    }
}
