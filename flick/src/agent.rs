use futures_util::future::join_all;
use tokio_stream::StreamExt;

use crate::config::Config;
use crate::context::{ContentBlock, Context};
use crate::error::FlickError;
use crate::event::{EventEmitter, RunSummary, StreamEvent};
use crate::provider::{DynProvider, RequestParams, ToolDefinition};
use crate::tool::ToolRegistry;

/// Maximum agent loop iterations. Not configurable; Epic controls iteration
/// budgets at the orchestration layer.
const DEFAULT_MAX_ITERATIONS: u32 = 25;

/// Run the agent loop: query model, execute tools, repeat until done.
#[allow(clippy::too_many_lines)]
pub async fn run(
    config: &Config,
    provider: &dyn DynProvider,
    tools: &ToolRegistry,
    context: &mut Context,
    emitter: &mut dyn EventEmitter,
) -> Result<(), FlickError> {
    let tool_defs = tools.definitions();
    let max_iterations = DEFAULT_MAX_ITERATIONS;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    for iteration in 1..=max_iterations {
        let params = build_params(config, &context.messages, tool_defs);
        let mut event_stream = provider.stream_boxed(params).await?;

        let mut assistant_content: Vec<ContentBlock> = Vec::new();
        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut current_thinking_signature = String::new();
        let mut pending_tool_calls: Vec<PendingToolCall> = Vec::new();

        while let Some(result) = event_stream.next().await {
            let event = result?;
            emitter.emit(&event);

            match event {
                StreamEvent::TextDelta { text } => {
                    current_text.push_str(&text);
                }
                StreamEvent::ThinkingDelta { text } => {
                    current_thinking.push_str(&text);
                }
                StreamEvent::ThinkingSignature { signature } => {
                    current_thinking_signature.push_str(&signature);
                }
                StreamEvent::ToolCallStart {
                    call_id,
                    tool_name,
                } => {
                    pending_tool_calls.push(PendingToolCall {
                        call_id,
                        tool_name,
                        arguments: String::new(),
                        // Defensive default — overwritten after streaming completes
                        parsed_input: Err("not yet parsed".into()),
                    });
                }
                // Defensive dual accumulation: arguments are accumulated here as fallback
                // in case a provider emits deltas but sends an empty `ToolCallEnd.arguments`.
                // Each provider's SSE parser also accumulates for the primary path.
                // Note: the ToolCallEnd event is emitted to the consumer *before* the
                // fallback assignment below, so the emitted event may have empty arguments
                // even though execution uses the accumulated args. This is intentional —
                // re-emitting after fixup would require special-casing in the emit loop.
                StreamEvent::ToolCallDelta {
                    call_id,
                    arguments_delta,
                } => {
                    if let Some(tc) =
                        pending_tool_calls.iter_mut().find(|t| t.call_id == call_id)
                    {
                        tc.arguments.push_str(&arguments_delta);
                    }
                }
                StreamEvent::ToolCallEnd {
                    call_id,
                    arguments,
                } => {
                    if let Some(tc) = pending_tool_calls.iter_mut().find(|t| t.call_id == call_id)
                    {
                        if !arguments.is_empty() {
                            tc.arguments = arguments;
                        }
                    }
                }
                // Accumulate usage. Provider Usage events are incremental (deltas), not cumulative.
                StreamEvent::Usage { input_tokens, output_tokens, .. } => {
                    total_input_tokens += input_tokens;
                    total_output_tokens += output_tokens;
                }
                // Fatal errors abort the agent loop. Non-fatal errors
                // (e.g. max_tokens truncation) are emitted to the consumer
                // but processing continues with whatever content was received.
                StreamEvent::Error { message, code, fatal } => {
                    if fatal {
                        return Err(FlickError::Provider(
                            crate::error::ProviderError::StreamError(
                                format!("stream error ({code}): {message}"),
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }

        if !current_thinking.is_empty() {
            assistant_content.push(ContentBlock::Thinking {
                text: current_thinking,
                signature: current_thinking_signature,
            });
        }

        // Finalize assistant text
        if !current_text.is_empty() {
            assistant_content.push(ContentBlock::Text {
                text: current_text,
            });
        }

        // Parse tool call arguments and add to assistant content
        for tc in &mut pending_tool_calls {
            tc.parsed_input = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                .map_err(|e| format!("invalid tool arguments JSON: {e}"));
            let input = match &tc.parsed_input {
                Ok(val) => val.clone(),
                Err(_) => serde_json::json!({"raw": tc.arguments}),
            };
            assistant_content.push(ContentBlock::ToolUse {
                id: tc.call_id.clone(),
                name: tc.tool_name.clone(),
                input,
            });
        }

        if !assistant_content.is_empty() {
            context.push_assistant(assistant_content)?;
        }

        // No tool calls → done
        if pending_tool_calls.is_empty() {
            emit_done(emitter, config, total_input_tokens, total_output_tokens, iteration);
            return Ok(());
        }

        // Execute tool calls concurrently
        let exec_results: Vec<(bool, String)> = join_all(
            pending_tool_calls.iter().map(|tc| async {
                match &tc.parsed_input {
                    Err(msg) => (false, msg.clone()),
                    Ok(parsed) => match tools.execute(&tc.tool_name, parsed).await {
                        Ok(out) => (true, out),
                        Err(e) => (false, e.to_string()),
                    },
                }
            }),
        )
        .await;

        let mut tool_results = Vec::new();
        for (tc, (success, output)) in pending_tool_calls.iter().zip(exec_results) {
            emitter.emit(&StreamEvent::ToolResult {
                call_id: tc.call_id.clone(),
                success,
                output: output.clone(),
            });

            tool_results.push(ContentBlock::ToolResult {
                tool_use_id: tc.call_id.clone(),
                content: output,
                is_error: !success,
            });
        }

        context.push_tool_results(tool_results)?;
    }

    // Do not emit Done on iteration limit — Done implies successful completion
    Err(FlickError::IterationLimit(max_iterations))
}

fn emit_done(
    emitter: &mut dyn EventEmitter,
    config: &Config,
    input_tokens: u64,
    output_tokens: u64,
    iterations: u32,
) {
    let usage = RunSummary {
        input_tokens,
        output_tokens,
        cost_usd: config.compute_cost(input_tokens, output_tokens),
        iterations,
    };
    emitter.emit(&StreamEvent::Done { usage });
}

pub fn build_params<'a>(
    config: &'a Config,
    messages: &'a [crate::context::Message],
    tool_defs: &'a [ToolDefinition],
) -> RequestParams<'a> {
    RequestParams {
        model: config.model().name(),
        max_tokens: config.model().max_tokens(),
        // Strip temperature when reasoning is active (provider-agnostic)
        temperature: if config.model().reasoning().is_some() {
            None
        } else {
            config.model().temperature()
        },
        system_prompt: config.system_prompt(),
        messages,
        tools: tool_defs,
        reasoning: config.model().reasoning().map(|r| r.level),
        output_schema: config.output_schema().map(|o| &o.schema),
    }
}

struct PendingToolCall {
    call_id: String,
    tool_name: String,
    arguments: String,
    /// Set after streaming completes; default `Err("not yet parsed")` is overwritten before use.
    parsed_input: Result<serde_json::Value, String>,
}

