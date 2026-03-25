// Librarian: reel agent invocation for vault operations.
//
// Defines two traits (DerivedProducer for write operations, QueryResponder for
// read-only queries) and the ReelLibrarian production implementation.
// Prompt composition lives in the sibling `prompts` module.

use crate::storage::Storage;
use crate::{Coverage, Extract, QueryResult};

use std::path::PathBuf;
use std::time::Duration;

pub const AGENT_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// Agent invocation traits
// ---------------------------------------------------------------------------

/// Trait for librarian invocations that produce or update derived documents.
/// Used by bootstrap and record operations.
pub trait DerivedProducer: Send + Sync {
    fn produce_derived(
        &self,
        system_prompt: &str,
        user_message: &str,
        storage: &Storage,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

/// Trait for librarian invocations that answer queries. Read-only: no files
/// are written. Returns a structured QueryResult parsed from the agent's
/// response.
pub trait QueryResponder: Send + Sync {
    fn answer_query(
        &self,
        system_prompt: &str,
        user_message: &str,
        storage: &Storage,
    ) -> impl std::future::Future<Output = Result<QueryResult, String>> + Send;
}

// ---------------------------------------------------------------------------
// Production implementation
// ---------------------------------------------------------------------------

/// Production invoker that delegates to a shared reel `Agent`.
pub struct ReelLibrarian<'a> {
    pub agent: &'a reel::Agent,
    pub model_name: &'a str,
}

impl ReelLibrarian<'_> {
    fn build_request(
        &self,
        system_prompt: &str,
        write_paths: Vec<PathBuf>,
    ) -> Result<reel::AgentRequestConfig, String> {
        let config = reel::RequestConfig::builder()
            .model(self.model_name)
            .system_prompt(system_prompt)
            .build()
            .map_err(|e| format!("failed to build request config: {e}"))?;

        Ok(reel::AgentRequestConfig {
            config,
            grant: reel::ToolGrant::TOOLS.normalize(),
            custom_tools: Vec::new(),
            write_paths,
        })
    }
}

impl DerivedProducer for ReelLibrarian<'_> {
    async fn produce_derived(
        &self,
        system_prompt: &str,
        user_message: &str,
        storage: &Storage,
    ) -> Result<(), String> {
        let request = self.build_request(system_prompt, vec![storage.derived_dir()])?;

        let _result: reel::RunResult<String> = self
            .agent
            .run(&request, user_message)
            .await
            .map_err(|e| format!("librarian agent failed: {e}"))?;

        Ok(())
    }
}

impl QueryResponder for ReelLibrarian<'_> {
    async fn answer_query(
        &self,
        system_prompt: &str,
        user_message: &str,
        _storage: &Storage,
    ) -> Result<QueryResult, String> {
        // Empty write_paths: reel only adds WRITE grant when write_paths is
        // non-empty (see reel agent.rs effective_tool_grant), so this is
        // genuinely read-only.
        let request = self.build_request(system_prompt, Vec::new())?;

        let result: reel::RunResult<String> = self
            .agent
            .run(&request, user_message)
            .await
            .map_err(|e| format!("librarian agent failed: {e}"))?;

        parse_query_response(&result.output)
    }
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Parse the agent's text response into a QueryResult.
///
/// Expects a JSON object with fields: coverage ("full"/"partial"/"none"),
/// answer (string), extracts (array of {content, source}).
fn parse_query_response(text: &str) -> Result<QueryResult, String> {
    let json_str = extract_json_block(text);

    let raw: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("failed to parse query JSON: {e}"))?;

    let coverage_str = raw
        .get("coverage")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'coverage' field")?;

    let coverage = match coverage_str {
        "full" => Coverage::Full,
        "partial" => Coverage::Partial,
        "none" => Coverage::None,
        other => return Err(format!("unknown coverage value: {other}")),
    };

    let answer = raw
        .get("answer")
        .and_then(|v| v.as_str())
        .ok_or("missing or invalid 'answer' field")?
        .to_owned();

    let extracts = match raw.get("extracts") {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .map(|item| {
                let content = item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or("extract missing 'content'")?
                    .to_owned();
                let source = item
                    .get("source")
                    .and_then(|v| v.as_str())
                    .ok_or("extract missing 'source'")?
                    .to_owned();
                Ok(Extract {
                    content,
                    source: crate::storage::DocumentRef { filename: source },
                })
            })
            .collect::<Result<Vec<_>, &str>>()
            .map_err(str::to_owned)?,
        Some(_) => return Err("'extracts' field must be an array".to_owned()),
        None => Vec::new(),
    };

    Ok(QueryResult {
        coverage,
        answer,
        extracts,
    })
}

/// Extract a JSON block from agent text. Handles optional markdown fences.
fn extract_json_block(text: &str) -> &str {
    // Try ```json fence first.
    if let Some(start) = text.find("```json") {
        let after_fence = &text[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim();
        }
    }
    // Try bare ``` fence (with either \n or \r\n after it).
    if let Some(start) = text.find("```") {
        let after_ticks = &text[start + 3..];
        // Skip the line ending after the opening fence.
        let content_start = if after_ticks.starts_with("\r\n") {
            2
        } else if after_ticks.starts_with('\n') {
            1
        } else {
            // No newline after ``` — not a fence block, fall through.
            return text.trim();
        };
        let content = &after_ticks[content_start..];
        if let Some(end) = content.find("```") {
            return content[..end].trim();
        }
    }
    text.trim()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_json() {
        let input = r#"{"coverage":"full","answer":"Yes.","extracts":[{"content":"detail","source":"PROJECT.md"}]}"#;
        let result = parse_query_response(input).unwrap();
        assert!(matches!(result.coverage, Coverage::Full));
        assert_eq!(result.answer, "Yes.");
        assert_eq!(result.extracts.len(), 1);
        assert_eq!(result.extracts[0].source.filename, "PROJECT.md");
    }

    #[test]
    fn parse_fenced_json() {
        let input = "Here is the answer:\n```json\n{\"coverage\":\"partial\",\"answer\":\"Maybe.\",\"extracts\":[]}\n```\n";
        let result = parse_query_response(input).unwrap();
        assert!(matches!(result.coverage, Coverage::Partial));
        assert_eq!(result.answer, "Maybe.");
        assert!(result.extracts.is_empty());
    }

    #[test]
    fn parse_bare_fence_without_language_tag() {
        let input = "```\n{\"coverage\":\"full\",\"answer\":\"Yes.\",\"extracts\":[]}\n```\n";
        let result = parse_query_response(input).unwrap();
        assert!(matches!(result.coverage, Coverage::Full));
        assert_eq!(result.answer, "Yes.");
    }

    #[test]
    fn parse_bare_fence_crlf() {
        let input = "```\r\n{\"coverage\":\"none\",\"answer\":\"No.\",\"extracts\":[]}\r\n```\r\n";
        let result = parse_query_response(input).unwrap();
        assert!(matches!(result.coverage, Coverage::None));
    }

    #[test]
    fn parse_none_coverage() {
        let input = r#"{"coverage":"none","answer":"No info.","extracts":[]}"#;
        let result = parse_query_response(input).unwrap();
        assert!(matches!(result.coverage, Coverage::None));
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse_query_response("not json");
        assert!(result.is_err());
    }

    #[test]
    fn parse_unknown_coverage() {
        let input = r#"{"coverage":"maybe","answer":"Hmm.","extracts":[]}"#;
        let result = parse_query_response(input);
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_answer() {
        let input = r#"{"coverage":"full","extracts":[]}"#;
        let result = parse_query_response(input);
        assert!(result.is_err());
    }

    #[test]
    fn parse_non_array_extracts() {
        let input = r#"{"coverage":"full","answer":"Yes.","extracts":"not-an-array"}"#;
        let result = parse_query_response(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be an array"));
    }

    #[test]
    fn parse_extract_missing_source() {
        let input = r#"{"coverage":"full","answer":"Yes.","extracts":[{"content":"x"}]}"#;
        let result = parse_query_response(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing 'source'"));
    }

    #[test]
    fn parse_extract_missing_content() {
        let input = r#"{"coverage":"full","answer":"Yes.","extracts":[{"source":"A.md"}]}"#;
        let result = parse_query_response(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing 'content'"));
    }
}
