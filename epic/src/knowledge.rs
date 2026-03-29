// Vault integration: ResearchTool (reel ToolHandler) with gap-filling pipeline.
//
// Pipeline: vault query -> gap identification -> codebase exploration -> synthesis.
// All internal agent calls use Haiku ("fast" model key). Exploration agents get
// read-only tools (ToolGrant::TOOLS). Gap identification and synthesis are
// structured-output calls with no tools.

use crate::agent::SessionMeta;
use serde::Deserialize;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// Maximum number of gaps to explore per research query. Prevents unbounded
/// agent spawning from a verbose LLM gap-analysis response.
const MAX_GAPS: usize = 5;

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
// ResearchScope — controls where the research service looks
// ---------------------------------------------------------------------------

/// Controls where the research service looks for information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchScope {
    /// Query vault documents only (no exploration).
    Vault,
    /// Query vault + explore project codebase to fill gaps.
    Project,
}

impl ResearchScope {
    /// Parse from the tool input's `scope` string. Defaults to `Project`.
    fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("vault") => Self::Vault,
            _ => Self::Project,
        }
    }
}

// ---------------------------------------------------------------------------
// ResearchResult — structured return type
// ---------------------------------------------------------------------------

/// Structured result from the research pipeline.
#[derive(Debug, Clone)]
pub struct ResearchResult {
    pub answer: String,
    pub document_refs: Vec<String>,
    pub gaps_filled: u32,
}

// ---------------------------------------------------------------------------
// Internal structured output types (serde, for Haiku calls)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GapAnalysis {
    gaps: Vec<String>,
    sufficient: bool,
}

#[derive(Debug, Deserialize)]
struct ExplorationResult {
    findings: Vec<Finding>,
}

#[derive(Debug, Deserialize)]
struct Finding {
    content: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct SynthesisResult {
    answer: String,
    #[serde(default)]
    document_refs: Vec<String>,
}

// ---------------------------------------------------------------------------
// JSON schemas for structured output calls
// ---------------------------------------------------------------------------

fn gap_analysis_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "gaps": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Specific information gaps that need to be filled by exploring the project codebase"
            },
            "sufficient": {
                "type": "boolean",
                "description": "Whether the existing knowledge is sufficient to answer the question"
            }
        },
        "required": ["gaps", "sufficient"]
    })
}

fn exploration_result_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The factual information discovered"
                        },
                        "source": {
                            "type": "string",
                            "description": "File path or description of where this was found"
                        }
                    },
                    "required": ["content", "source"]
                },
                "description": "Factual findings from codebase exploration"
            }
        },
        "required": ["findings"]
    })
}

fn synthesis_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "answer": {
                "type": "string",
                "description": "Synthesized answer to the research question"
            },
            "document_refs": {
                "type": "array",
                "items": { "type": "string" },
                "description": "References to vault document sections (FILENAME > Section format)"
            }
        },
        "required": ["answer", "document_refs"]
    })
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format vault extracts as `[filename]: content` lines.
fn format_extracts(extracts: &[vault::Extract]) -> String {
    if extracts.is_empty() {
        "(no extracts)".to_string()
    } else {
        extracts
            .iter()
            .map(|e| format!("[{}]: {}", e.source.filename, e.content))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Format exploration findings as `[source]: content` lines.
fn format_findings(findings: &[Finding]) -> String {
    if findings.is_empty() {
        "(no exploration findings)".to_string()
    } else {
        findings
            .iter()
            .map(|f| format!("[{}]: {}", f.source, f.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

// ---------------------------------------------------------------------------
// ResearchTool — reel ToolHandler with gap-filling pipeline
// ---------------------------------------------------------------------------

/// Custom reel tool that exposes the research service to agents.
///
/// Pipeline: vault query -> gap identification -> codebase exploration -> synthesis.
/// All internal agent calls use Haiku. Exploration agents get read-only tools.
pub struct ResearchTool {
    vault: Arc<vault::Vault>,
    agent: Arc<reel::Agent>,
    usage_sink: Arc<Mutex<Vec<SessionMeta>>>,
}

impl ResearchTool {
    /// Push a `SessionMeta` into the usage sink.
    fn sink_usage(&self, meta: SessionMeta) {
        self.usage_sink.lock().unwrap().push(meta);
    }

    /// Run a Haiku agent call with structured output.
    async fn run_haiku<T: serde::de::DeserializeOwned>(
        &self,
        system_prompt: &str,
        query: &str,
        schema: serde_json::Value,
        grant: reel::ToolGrant,
    ) -> anyhow::Result<T> {
        let config = reel::RequestConfig::builder()
            .model("fast")
            .system_prompt(system_prompt)
            .output_schema(schema)
            .build()
            .map_err(|e| anyhow::anyhow!("request config: {e}"))?;

        let request = reel::AgentRequestConfig {
            config,
            grant,
            custom_tools: Vec::new(),
            write_paths: Vec::new(),
        };

        let result: reel::RunResult<T> = self.agent.run(&request, query).await?;
        self.sink_usage(SessionMeta::from_run_result(&result));
        Ok(result.output)
    }

    // -----------------------------------------------------------------------
    // Step 2: Gap identification (Haiku, no tools, structured output)
    // -----------------------------------------------------------------------

    async fn identify_gaps(
        &self,
        question: &str,
        query_result: &vault::QueryResult,
    ) -> anyhow::Result<GapAnalysis> {
        let coverage_label = match query_result.coverage {
            vault::Coverage::Full => "Full",
            vault::Coverage::Partial => "Partial",
            vault::Coverage::None => "None",
        };

        let extracts_text = format_extracts(&query_result.extracts);

        let system_prompt = "You are an information gap analyst. Given a question and \
            existing knowledge from a document store, identify specific information gaps \
            that need to be filled by exploring the project codebase. Focus on gaps that \
            would cause wrong decisions if not filled. Do not list nice-to-have information. \
            Set sufficient=true if the existing knowledge adequately answers the question.";

        let query = format!(
            "QUESTION:\n{question}\n\n\
             COVERAGE: {coverage_label}\n\n\
             EXISTING ANSWER:\n{answer}\n\n\
             SUPPORTING EXTRACTS:\n{extracts_text}\n\n\
             Identify what specific information gaps remain.",
            answer = query_result.answer,
        );

        self.run_haiku(
            system_prompt,
            &query,
            gap_analysis_schema(),
            reel::ToolGrant::empty(),
        )
        .await
    }

    // -----------------------------------------------------------------------
    // Step 3: Codebase exploration (Haiku, read-only tools)
    // -----------------------------------------------------------------------

    async fn explore_codebase(&self, question: &str, gap: &str) -> anyhow::Result<Vec<Finding>> {
        let system_prompt = "You are a code exploration agent. Your job is to explore the \
            project codebase to find specific information. Report only factual observations \
            from the code. Do not speculate or make recommendations. For each finding, note \
            the source file path.";

        let query = format!(
            "RESEARCH QUESTION:\n{question}\n\n\
             SPECIFIC GAP TO FILL:\n{gap}\n\n\
             Explore the codebase using the available tools (Read, Glob, Grep, NuShell) \
             to find information that addresses the gap above. Report your findings.",
        );

        let result: ExplorationResult = self
            .run_haiku(
                system_prompt,
                &query,
                exploration_result_schema(),
                reel::ToolGrant::TOOLS,
            )
            .await?;
        Ok(result.findings)
    }

    // -----------------------------------------------------------------------
    // Record findings into vault (best-effort)
    // -----------------------------------------------------------------------

    async fn record_findings(&self, question: &str, findings: &[Finding]) {
        let content = findings
            .iter()
            .map(|f| format!("### {}\n\n{}", f.source, f.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        let name = format!(
            "RESEARCH_{}",
            question
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == ' ')
                .take(40)
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("_")
                .to_uppercase()
        );

        let result = self
            .vault
            .record(&name, &content, vault::RecordMode::New)
            .await;
        let result = match result {
            Err(vault::RecordError::VersionConflict(_)) => {
                self.vault
                    .record(&name, &content, vault::RecordMode::Append)
                    .await
            }
            other => other,
        };
        match result {
            Ok((_refs, _warnings, meta)) => {
                self.sink_usage(SessionMeta::from_vault(&meta));
            }
            Err(e) => {
                eprintln!("Research: failed to record findings: {e}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 4: Synthesis (Haiku, no tools, structured output)
    // -----------------------------------------------------------------------

    async fn synthesize(
        &self,
        question: &str,
        query_result: &vault::QueryResult,
        exploration_findings: &[Finding],
    ) -> anyhow::Result<SynthesisResult> {
        let existing_knowledge = if query_result.answer.is_empty() {
            "(no existing knowledge)".to_string()
        } else {
            let extracts_text = format_extracts(&query_result.extracts);
            format!(
                "{}\n\nSupporting extracts:\n{}",
                query_result.answer, extracts_text
            )
        };

        let findings_text = format_findings(exploration_findings);

        let system_prompt = "You are a research synthesizer. Combine existing knowledge \
            and exploration findings into a comprehensive answer. Use only the provided \
            information. If information is insufficient, state what is missing. Include \
            document_refs listing the vault document sections that contributed to the answer \
            (FILENAME > Section format).";

        let query = format!(
            "QUESTION:\n{question}\n\n\
             EXISTING KNOWLEDGE:\n{existing_knowledge}\n\n\
             EXPLORATION FINDINGS:\n{findings_text}\n\n\
             Synthesize a comprehensive answer from the information above.",
        );

        self.run_haiku(
            system_prompt,
            &query,
            synthesis_schema(),
            reel::ToolGrant::empty(),
        )
        .await
    }
}

impl reel::ToolHandler for ResearchTool {
    fn definition(&self) -> reel::ToolDefinition {
        reel::ToolDefinition {
            name: "ResearchQuery".into(),
            description: "Query the project knowledge base for accumulated research, \
                          discoveries, requirements, and design decisions. When vault \
                          knowledge is insufficient, explores the project codebase to \
                          fill gaps. Use when you need context about the project or \
                          answers to questions about prior work."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to research against accumulated project knowledge"
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["vault", "project"],
                        "description": "Where to look. 'vault' = stored knowledge only. 'project' = vault + codebase exploration to fill gaps. Default: project."
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

            let scope =
                ResearchScope::from_str_opt(input.get("scope").and_then(serde_json::Value::as_str));

            match self.run_pipeline(question, scope).await {
                Ok(result) => reel::ToolExecResult {
                    tool_use_id,
                    content: format_research_result(&result),
                    is_error: false,
                },
                Err(e) => reel::ToolExecResult {
                    tool_use_id,
                    content: format!("Research query failed: {e}"),
                    is_error: true,
                },
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Pipeline orchestrator
// ---------------------------------------------------------------------------

/// Build a vault-only `ResearchResult` from a `QueryResult` (no exploration).
fn vault_only_result(query_result: &vault::QueryResult) -> ResearchResult {
    let document_refs = query_result
        .extracts
        .iter()
        .map(|e| e.source.filename.clone())
        .collect();
    ResearchResult {
        answer: query_result.answer.clone(),
        document_refs,
        gaps_filled: 0,
    }
}

impl ResearchTool {
    /// Run the full research pipeline.
    async fn run_pipeline(
        &self,
        question: &str,
        scope: ResearchScope,
    ) -> anyhow::Result<ResearchResult> {
        // Step 1: Query vault
        let (query_result, meta) = self.vault.query(question).await?;
        self.sink_usage(SessionMeta::from_vault(&meta));

        // Short-circuit: full coverage or vault-only scope
        if query_result.coverage == vault::Coverage::Full || scope == ResearchScope::Vault {
            return Ok(vault_only_result(&query_result));
        }

        // Step 2: Gap identification
        let gap_analysis = match self.identify_gaps(question, &query_result).await {
            Ok(ga) => ga,
            Err(e) => {
                eprintln!("Research: gap identification failed: {e}");
                return Ok(vault_only_result(&query_result));
            }
        };

        if gap_analysis.sufficient || gap_analysis.gaps.is_empty() {
            return Ok(vault_only_result(&query_result));
        }

        // Step 3: Codebase exploration (sequential, capped at MAX_GAPS)
        let mut gaps_filled = 0u32;
        let mut all_findings: Vec<Finding> = Vec::new();

        for gap in gap_analysis.gaps.iter().take(MAX_GAPS) {
            match self.explore_codebase(question, gap).await {
                Ok(findings) => {
                    if !findings.is_empty() {
                        self.record_findings(question, &findings).await;
                        all_findings.extend(findings);
                        gaps_filled += 1;
                    }
                }
                Err(e) => {
                    eprintln!("Research: exploration failed for gap '{gap}': {e}");
                }
            }
        }

        // Step 4: Synthesis
        match self
            .synthesize(question, &query_result, &all_findings)
            .await
        {
            Ok(synthesis) => Ok(ResearchResult {
                answer: synthesis.answer,
                document_refs: synthesis.document_refs,
                gaps_filled,
            }),
            Err(e) => {
                // Fall back to vault answer + raw findings on synthesis failure
                eprintln!("Research: synthesis failed: {e}");
                let mut answer = query_result.answer.clone();
                if !all_findings.is_empty() {
                    answer.push_str("\n\n--- Exploration Findings ---\n");
                    answer.push_str(&format_findings(&all_findings));
                }
                let mut result = vault_only_result(&query_result);
                result.answer = answer;
                result.gaps_filled = gaps_filled;
                Ok(result)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

/// Format a `ResearchResult` as readable text for the agent.
fn format_research_result(result: &ResearchResult) -> String {
    let mut out = String::new();

    let _ = write!(out, "Gaps filled: {}\n\n", result.gaps_filled);
    out.push_str(&result.answer);

    if !result.document_refs.is_empty() {
        out.push_str("\n\n--- Document References ---\n");
        for doc_ref in &result.document_refs {
            let _ = writeln!(out, "- {doc_ref}");
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
/// sink handle (drain after agent run to fold research costs into task usage).
///
/// The `agent` parameter is the reel Agent used for internal Haiku calls
/// (gap identification, codebase exploration, synthesis).
pub fn build_research_tool(
    vault: &Arc<vault::Vault>,
    agent: &Arc<reel::Agent>,
) -> (Box<dyn reel::ToolHandler>, Arc<Mutex<Vec<SessionMeta>>>) {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let tool = ResearchTool {
        vault: vault.clone(),
        agent: agent.clone(),
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
    fn research_tool_definition_schema() {
        let vault = make_dummy_vault();
        let agent = make_dummy_agent();
        let (tool, _sink) = build_research_tool(&vault, &agent);
        let def = tool.definition();
        assert_eq!(def.name, "ResearchQuery");
        let props = def.parameters.get("properties").unwrap();
        assert!(props.get("question").is_some());
        assert!(props.get("scope").is_some());
        let required = def.parameters.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("question")));
        // scope is optional
        assert!(!required.iter().any(|v| v.as_str() == Some("scope")));
    }

    // -----------------------------------------------------------------------
    // ResearchScope parsing
    // -----------------------------------------------------------------------

    #[test]
    fn scope_from_str_defaults_to_project() {
        assert_eq!(ResearchScope::from_str_opt(None), ResearchScope::Project);
        assert_eq!(
            ResearchScope::from_str_opt(Some("unknown")),
            ResearchScope::Project
        );
        assert_eq!(
            ResearchScope::from_str_opt(Some("")),
            ResearchScope::Project
        );
    }

    #[test]
    fn scope_from_str_vault() {
        assert_eq!(
            ResearchScope::from_str_opt(Some("vault")),
            ResearchScope::Vault
        );
    }

    // -----------------------------------------------------------------------
    // Structured output parsing
    // -----------------------------------------------------------------------

    #[test]
    fn gap_analysis_deserialize() {
        let json =
            r#"{"gaps": ["missing API docs", "no error handling info"], "sufficient": false}"#;
        let ga: GapAnalysis = serde_json::from_str(json).unwrap();
        assert_eq!(ga.gaps.len(), 2);
        assert!(!ga.sufficient);
    }

    #[test]
    fn gap_analysis_deserialize_sufficient() {
        let json = r#"{"gaps": [], "sufficient": true}"#;
        let ga: GapAnalysis = serde_json::from_str(json).unwrap();
        assert!(ga.gaps.is_empty());
        assert!(ga.sufficient);
    }

    #[test]
    fn exploration_result_deserialize() {
        let json = r#"{"findings": [{"content": "uses tokio runtime", "source": "src/main.rs"}]}"#;
        let er: ExplorationResult = serde_json::from_str(json).unwrap();
        assert_eq!(er.findings.len(), 1);
        assert_eq!(er.findings[0].source, "src/main.rs");
    }

    #[test]
    fn exploration_result_deserialize_empty() {
        let json = r#"{"findings": []}"#;
        let er: ExplorationResult = serde_json::from_str(json).unwrap();
        assert!(er.findings.is_empty());
    }

    #[test]
    fn synthesis_result_deserialize() {
        let json =
            r#"{"answer": "The system uses DFS.", "document_refs": ["DESIGN.md > Architecture"]}"#;
        let sr: SynthesisResult = serde_json::from_str(json).unwrap();
        assert_eq!(sr.answer, "The system uses DFS.");
        assert_eq!(sr.document_refs.len(), 1);
    }

    #[test]
    fn synthesis_result_deserialize_default_refs() {
        let json = r#"{"answer": "Some answer."}"#;
        let sr: SynthesisResult = serde_json::from_str(json).unwrap();
        assert!(sr.document_refs.is_empty());
    }

    // -----------------------------------------------------------------------
    // Format helpers
    // -----------------------------------------------------------------------

    #[test]
    fn format_extracts_empty() {
        let formatted = format_extracts(&[]);
        assert_eq!(formatted, "(no extracts)");
    }

    #[test]
    fn format_extracts_with_items() {
        let extracts = vec![vault::Extract {
            content: "DFS is used.".into(),
            source: vault::DocumentRef {
                filename: "DESIGN.md".into(),
            },
        }];
        let formatted = format_extracts(&extracts);
        assert!(formatted.contains("[DESIGN.md]: DFS is used."));
    }

    #[test]
    fn format_findings_empty() {
        let formatted = format_findings(&[]);
        assert_eq!(formatted, "(no exploration findings)");
    }

    #[test]
    fn format_findings_with_items() {
        let findings = vec![Finding {
            content: "uses tokio".into(),
            source: "src/main.rs".into(),
        }];
        let formatted = format_findings(&findings);
        assert!(formatted.contains("[src/main.rs]: uses tokio"));
    }

    // -----------------------------------------------------------------------
    // Format research result
    // -----------------------------------------------------------------------

    #[test]
    fn format_research_result_with_refs() {
        let result = ResearchResult {
            answer: "The system uses DFS traversal.".into(),
            document_refs: vec!["DESIGN.md > Architecture".into()],
            gaps_filled: 2,
        };
        let formatted = format_research_result(&result);
        assert!(formatted.contains("Gaps filled: 2"));
        assert!(formatted.contains("The system uses DFS traversal."));
        assert!(formatted.contains("DESIGN.md > Architecture"));
    }

    #[test]
    fn format_research_result_no_refs() {
        let result = ResearchResult {
            answer: "No info found.".into(),
            document_refs: vec![],
            gaps_filled: 0,
        };
        let formatted = format_research_result(&result);
        assert!(formatted.contains("Gaps filled: 0"));
        assert!(!formatted.contains("Document References"));
    }

    // -----------------------------------------------------------------------
    // vault_only_result
    // -----------------------------------------------------------------------

    #[test]
    fn vault_only_result_maps_extracts_to_refs() {
        let qr = vault::QueryResult {
            coverage: vault::Coverage::Partial,
            answer: "Partial answer.".into(),
            extracts: vec![vault::Extract {
                content: "extract".into(),
                source: vault::DocumentRef {
                    filename: "DOC.md".into(),
                },
            }],
        };
        let result = vault_only_result(&qr);
        assert_eq!(result.answer, "Partial answer.");
        assert_eq!(result.document_refs, vec!["DOC.md"]);
        assert_eq!(result.gaps_filled, 0);
    }

    // -----------------------------------------------------------------------
    // Schema validation
    // -----------------------------------------------------------------------

    #[test]
    fn schemas_are_valid_json_objects() {
        let cases: Vec<(&str, serde_json::Value, &[&str])> = vec![
            (
                "gap_analysis",
                gap_analysis_schema(),
                &["gaps", "sufficient"],
            ),
            (
                "exploration_result",
                exploration_result_schema(),
                &["findings"],
            ),
            (
                "synthesis",
                synthesis_schema(),
                &["answer", "document_refs"],
            ),
        ];
        for (name, schema, expected_props) in &cases {
            assert_eq!(schema["type"], "object", "{name} schema not object type");
            for prop in *expected_props {
                assert!(
                    schema["properties"][prop].is_object(),
                    "{name} schema missing property '{prop}'"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

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

    fn make_dummy_agent() -> Arc<reel::Agent> {
        let model_registry =
            reel::ModelRegistry::from_map(std::collections::BTreeMap::new()).unwrap();
        let provider_registry = reel::ProviderRegistry::load_default().unwrap();
        let env = reel::AgentEnvironment {
            model_registry,
            provider_registry,
            project_root: std::path::PathBuf::from("."),
            timeout: std::time::Duration::from_secs(30),
        };
        Arc::new(reel::Agent::new(env))
    }

    #[test]
    fn max_gaps_cap_is_five() {
        assert_eq!(MAX_GAPS, 5);

        // Verify that .take(MAX_GAPS) on a longer list produces exactly 5.
        let gaps: Vec<String> = (0..10).map(|i| format!("gap {i}")).collect();
        assert_eq!(gaps.iter().take(MAX_GAPS).count(), 5);
    }
}
