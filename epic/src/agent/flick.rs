// FlickAgent: AgentService implementation backed by the Flick executable.

use crate::agent::config_gen::{
    self, AssessmentWire, CheckpointWire, DecompositionWire, RecoveryWire, TaskOutcomeWire,
    VerificationWire,
};
use crate::agent::prompts;
use crate::agent::tools::{self, AgentMethod, ToolGrant};
use crate::agent::{AgentService, TaskContext};
use crate::task::assess::AssessmentResult;
use crate::task::branch::{CheckpointDecision, DecompositionResult};
use crate::task::verify::VerificationResult;
use crate::task::{Model, TaskOutcome};
use anyhow::{Context, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const MAX_TOOL_ROUNDS: u32 = 50;

/// `FlickAgent` invokes the Flick executable for each agent call.
#[allow(dead_code)] // Fields used by invoke_flick and run methods.
pub struct FlickAgent {
    flick_path: PathBuf,
    project_root: PathBuf,
    work_dir: PathBuf,
    credential_name: String,
    call_timeout: Duration,
}

// ---------------------------------------------------------------------------
// Flick output types (deserialized from stdout)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FlickOutput {
    status: String,
    content: Option<Vec<ContentBlock>>,
    #[allow(dead_code)]
    usage: Option<UsageSummary>,
    context_hash: Option<String>,
    error: Option<FlickError>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {
        #[allow(dead_code)]
        text: String,
        #[allow(dead_code)]
        signature: Option<String>,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: JsonValue,
    },
    /// Catch-all for unknown/future content block types.
    /// Prevents deserialization failure on unrecognized types.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UsageSummary {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct FlickError {
    message: String,
    #[allow(dead_code)]
    code: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool result file format for --tool-results
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
struct ToolResultEntry {
    tool_use_id: String,
    content: String,
    is_error: bool,
}

impl FlickAgent {
    pub async fn new(
        flick_path: PathBuf,
        project_root: PathBuf,
        work_dir: PathBuf,
        credential_name: String,
        call_timeout: Duration,
    ) -> anyhow::Result<Self> {
        tokio::fs::create_dir_all(&work_dir)
            .await
            .with_context(|| format!("failed to create work dir: {}", work_dir.display()))?;
        Ok(Self {
            flick_path,
            project_root,
            work_dir,
            credential_name,
            call_timeout,
        })
    }

    // -----------------------------------------------------------------------
    // Core: invoke the flick binary
    // -----------------------------------------------------------------------

    async fn invoke_flick(&self, args: &[&str]) -> anyhow::Result<FlickOutput> {
        let mut cmd = Command::new(&self.flick_path);
        cmd.args(args)
            .current_dir(&self.project_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let child = cmd
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!("failed to spawn flick at {}", self.flick_path.display())
            })?;

        let output = tokio::time::timeout(self.call_timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                // On timeout, the Child is dropped here, and kill_on_drop(true) ensures
                // the process is killed rather than becoming an orphan.
                anyhow::anyhow!("flick call timed out after {:?}", self.call_timeout)
            })?
            .context("flick process failed")?;

        let stdout = String::from_utf8(output.stdout)
            .context("flick stdout is not valid UTF-8")?;

        if !output.status.success() {
            // Flick may write structured error JSON to stdout even on non-zero exit.
            // Try to parse it so callers get the structured error message.
            if let Ok(flick_output) = serde_json::from_str::<FlickOutput>(&stdout) {
                if flick_output.status == "error" {
                    let msg = flick_output
                        .error
                        .map_or_else(|| "unknown flick error".into(), |e| e.message);
                    bail!("flick returned error: {msg}");
                }
            }
            // Stdout wasn't parseable (or wasn't an error status); fall back to stderr.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit_info = format_exit_status(output.status);
            bail!("flick {exit_info}: {stderr}");
        }

        let flick_output: FlickOutput = serde_json::from_str(&stdout).with_context(|| {
            format!("failed to parse flick JSON output: {stdout}")
        })?;

        if flick_output.status == "error" {
            let msg = flick_output
                .error
                .map_or_else(|| "unknown flick error".into(), |e| e.message);
            bail!("flick returned error: {msg}");
        }

        Ok(flick_output)
    }

    // -----------------------------------------------------------------------
    // run_structured: single call, no tools, parse structured output
    // -----------------------------------------------------------------------

    async fn run_structured<T: DeserializeOwned>(
        &self,
        config_path: &Path,
        query: &str,
    ) -> anyhow::Result<T> {
        let config_str = config_path.to_str().context("config path not UTF-8")?;
        let output = self
            .invoke_flick(&["run", "--config", config_str, "--query", query])
            .await?;

        let text = extract_text(&output)?;
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse structured output: {text}"))
    }

    // -----------------------------------------------------------------------
    // run_with_tools: tool loop until complete
    // -----------------------------------------------------------------------

    async fn run_with_tools<T: DeserializeOwned>(
        &self,
        config_path: &Path,
        query: &str,
        grant: ToolGrant,
        task_id: u64,
        method: &str,
    ) -> anyhow::Result<T> {
        let config_str = config_path.to_str().context("config path not UTF-8")?;

        // Initial call (not counted toward MAX_TOOL_ROUNDS; the limit applies to resume rounds).
        let mut output = self
            .invoke_flick(&["run", "--config", config_str, "--query", query])
            .await?;

        for round in 1..=MAX_TOOL_ROUNDS {
            if output.status != "tool_calls_pending" {
                break;
            }

            let tool_calls = extract_tool_calls(&output)?;
            let mut results = Vec::with_capacity(tool_calls.len());
            for (id, name, input) in &tool_calls {
                let r = tools::execute_tool(
                    id.clone(),
                    name,
                    input,
                    &self.project_root,
                    grant,
                )
                .await;
                results.push(ToolResultEntry {
                    tool_use_id: r.tool_use_id,
                    content: r.content,
                    is_error: r.is_error,
                });
            }

            // Write tool results to a temp file
            let results_filename = format!("{task_id}_{method}_tools_{round}.json");
            let results_path = self.work_dir.join(results_filename);
            let results_json = serde_json::to_string(&results)
                .context("failed to serialize tool results")?;
            tokio::fs::write(&results_path, &results_json).await
                .context("failed to write tool results file")?;

            let context_hash = output
                .context_hash
                .as_deref()
                .context("tool_calls_pending but no context_hash")?;

            let results_str = results_path.to_str().context("results path not UTF-8")?;
            output = self
                .invoke_flick(&[
                    "run",
                    "--config",
                    config_str,
                    "--resume",
                    context_hash,
                    "--tool-results",
                    results_str,
                ])
                .await?;
        }

        if output.status == "tool_calls_pending" {
            bail!("flick tool loop exceeded {MAX_TOOL_ROUNDS} rounds");
        }

        let text = extract_text(&output)?;
        serde_json::from_str(&text)
            .with_context(|| format!("failed to parse structured output: {text}"))
    }
}

// ---------------------------------------------------------------------------
// AgentService implementation
// ---------------------------------------------------------------------------

impl AgentService for FlickAgent {
    async fn assess(&self, ctx: &TaskContext) -> anyhow::Result<AssessmentResult> {
        let pair = prompts::build_assess(ctx);
        let grant = tools::phase_tools(AgentMethod::Analyze);
        let config = config_gen::build_assess_config(
            pair.system_prompt,
            Model::Sonnet,
            &self.credential_name,
            grant,
        );
        let config_path =
            config_gen::write_config(&config, &self.work_dir, ctx.task.id.0, "assess").await?;

        let wire: AssessmentWire = self.run_structured(&config_path, &pair.query).await?;
        AssessmentResult::try_from(wire)
    }

    async fn execute_leaf(
        &self,
        ctx: &TaskContext,
        model: Model,
    ) -> anyhow::Result<TaskOutcome> {
        let pair = prompts::build_execute_leaf(ctx);
        let grant = tools::phase_tools(AgentMethod::Execute);
        let config = config_gen::build_execute_leaf_config(
            pair.system_prompt,
            model,
            &self.credential_name,
            grant,
        );
        let config_path =
            config_gen::write_config(&config, &self.work_dir, ctx.task.id.0, "execute").await?;

        let wire: TaskOutcomeWire = self
            .run_with_tools(&config_path, &pair.query, grant, ctx.task.id.0, "execute")
            .await?;
        TaskOutcome::try_from(wire)
    }

    async fn design_and_decompose(
        &self,
        ctx: &TaskContext,
    ) -> anyhow::Result<DecompositionResult> {
        let pair = prompts::build_design_and_decompose(ctx);
        let grant = tools::phase_tools(AgentMethod::Decompose);
        let config = config_gen::build_decompose_config(
            pair.system_prompt,
            Model::Sonnet,
            &self.credential_name,
            grant,
        );
        let config_path =
            config_gen::write_config(&config, &self.work_dir, ctx.task.id.0, "decompose").await?;

        let wire: DecompositionWire = self
            .run_with_tools(&config_path, &pair.query, grant, ctx.task.id.0, "decompose")
            .await?;
        DecompositionResult::try_from(wire)
    }

    async fn verify(&self, ctx: &TaskContext) -> anyhow::Result<VerificationResult> {
        let pair = prompts::build_verify(ctx);
        let grant = tools::phase_tools(AgentMethod::Analyze);
        let config = config_gen::build_verify_config(
            pair.system_prompt,
            Model::Sonnet,
            &self.credential_name,
            grant,
        );
        let config_path =
            config_gen::write_config(&config, &self.work_dir, ctx.task.id.0, "verify").await?;

        let wire: VerificationWire = self
            .run_with_tools(&config_path, &pair.query, grant, ctx.task.id.0, "verify")
            .await?;
        VerificationResult::try_from(wire)
    }

    async fn checkpoint(
        &self,
        ctx: &TaskContext,
        discoveries: &[String],
    ) -> anyhow::Result<CheckpointDecision> {
        let pair = prompts::build_checkpoint(ctx, discoveries);
        let config = config_gen::build_checkpoint_config(
            pair.system_prompt,
            Model::Sonnet,
            &self.credential_name,
        );
        let config_path =
            config_gen::write_config(&config, &self.work_dir, ctx.task.id.0, "checkpoint").await?;

        let wire: CheckpointWire = self.run_structured(&config_path, &pair.query).await?;
        CheckpointDecision::try_from(wire)
    }

    async fn assess_recovery(
        &self,
        ctx: &TaskContext,
        failure_reason: &str,
    ) -> anyhow::Result<Option<String>> {
        let pair = prompts::build_assess_recovery(ctx, failure_reason);
        let config = config_gen::build_recovery_config(
            pair.system_prompt,
            Model::Sonnet,
            &self.credential_name,
        );
        let config_path =
            config_gen::write_config(&config, &self.work_dir, ctx.task.id.0, "recovery").await?;

        let wire: RecoveryWire = self.run_structured(&config_path, &pair.query).await?;
        Ok(wire.into())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_text(output: &FlickOutput) -> anyhow::Result<String> {
    let blocks = output
        .content
        .as_ref()
        .context("flick output has no content blocks")?;

    let mut last_text: Option<&str> = None;
    for block in blocks {
        if let ContentBlock::Text { text } = block {
            last_text = Some(text.as_str());
        }
    }

    last_text
        .map(ToOwned::to_owned)
        .context("no text block found in flick output")
}

fn format_exit_status(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exited with status {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    format!("exited with unknown status: {status}")
}

fn extract_tool_calls(output: &FlickOutput) -> anyhow::Result<Vec<(String, String, JsonValue)>> {
    let blocks = output
        .content
        .as_ref()
        .context("flick output has no content blocks")?;

    let calls: Vec<_> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => {
                Some((id.clone(), name.clone(), input.clone()))
            }
            _ => None,
        })
        .collect();

    if calls.is_empty() {
        bail!("tool_calls_pending but no tool_use blocks found");
    }

    Ok(calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_from_blocks() {
        let output = FlickOutput {
            status: "complete".into(),
            content: Some(vec![
                ContentBlock::Thinking {
                    text: "hmm".into(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "Here is my analysis of the task.".into(),
                },
                ContentBlock::Text {
                    text: r#"{"path":"leaf","model":"haiku","rationale":"simple"}"#.into(),
                },
            ]),
            usage: None,
            context_hash: None,
            error: None,
        };
        let text = extract_text(&output).unwrap();
        // Must return the last text block, where structured JSON output lives.
        assert!(text.contains("leaf"));
        assert!(!text.contains("analysis"));
    }

    #[test]
    fn extract_text_missing() {
        let output = FlickOutput {
            status: "complete".into(),
            content: Some(vec![ContentBlock::Thinking {
                text: "hmm".into(),
                signature: None,
            }]),
            usage: None,
            context_hash: None,
            error: None,
        };
        assert!(extract_text(&output).is_err());
    }

    #[test]
    fn extract_tool_calls_from_blocks() {
        let output = FlickOutput {
            status: "tool_calls_pending".into(),
            content: Some(vec![
                ContentBlock::Text {
                    text: "let me check".into(),
                },
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "src/main.rs"}),
                },
            ]),
            usage: None,
            context_hash: Some("abc123".into()),
            error: None,
        };
        let calls = extract_tool_calls(&output).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "tu_1");
        assert_eq!(calls[0].1, "read_file");
    }

    #[test]
    fn flick_output_deserialize() {
        let json = r#"{
            "status": "complete",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "thinking", "text": "pondering", "signature": "sig123"},
                {"type": "tool_use", "id": "tu_1", "name": "bash", "input": {"command": "ls"}}
            ],
            "usage": {"input_tokens": 100, "output_tokens": 50},
            "context_hash": "hash123",
            "error": null
        }"#;
        let output: FlickOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.status, "complete");
        assert_eq!(output.content.as_ref().unwrap().len(), 3);
        assert_eq!(output.context_hash, Some("hash123".into()));
    }

    #[test]
    fn unknown_content_block_type_is_tolerated() {
        let json = r#"{
            "status": "complete",
            "content": [
                {"type": "image", "source": {"data": "base64..."}},
                {"type": "text", "text": "result"}
            ],
            "usage": null,
            "context_hash": null,
            "error": null
        }"#;
        let output: FlickOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.content.as_ref().unwrap().len(), 2);
        let text = extract_text(&output).unwrap();
        assert_eq!(text, "result");
    }

    #[test]
    fn flick_error_output_deserialize() {
        let json = r#"{
            "status": "error",
            "content": null,
            "usage": null,
            "context_hash": null,
            "error": {"message": "rate limited", "code": "429"}
        }"#;
        let output: FlickOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.status, "error");
        assert_eq!(output.error.unwrap().message, "rate limited");
    }

    #[test]
    fn tool_result_entry_serializes() {
        let entry = ToolResultEntry {
            tool_use_id: "tu_1".into(),
            content: "file contents here".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("tu_1"));
        assert!(json.contains("file contents here"));
    }
}
