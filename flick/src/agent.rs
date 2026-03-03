use futures_util::future::join_all;
use xxhash_rust::xxh3::xxh3_128;

use crate::config::Config;
use crate::context::{ContentBlock, Context};
use crate::error::FlickError;
use crate::event::{EventEmitter, RunSummary, Event};
use crate::provider::{DynProvider, ModelResponse, RequestParams, ToolDefinition};
use crate::tool::ToolRegistry;

/// Maximum agent loop iterations. Not configurable; Epic controls iteration
/// budgets at the orchestration layer.
const DEFAULT_MAX_ITERATIONS: u32 = 25;

/// Run the agent loop: query model, execute tools, repeat until done.
pub async fn run(
    config: &Config,
    provider: &dyn DynProvider,
    tools: &ToolRegistry,
    context: &mut Context,
    emitter: &mut dyn EventEmitter,
) -> Result<RunSummary, FlickError> {
    let tool_defs = tools.definitions();
    let max_iterations = DEFAULT_MAX_ITERATIONS;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    for iteration in 1..=max_iterations {
        let params = build_params(config, &context.messages, tool_defs);
        let response = provider.call_boxed(params).await?;

        total_input_tokens += response.usage.input_tokens;
        total_output_tokens += response.usage.output_tokens;

        emit_response_events(&response, emitter);
        let assistant_content = build_assistant_content(&response);

        if !assistant_content.is_empty() {
            context.push_assistant(assistant_content)?;
        }

        if response.tool_calls.is_empty() {
            let context_hash = context_hash(context);
            let summary = emit_done(emitter, config, total_input_tokens, total_output_tokens, iteration, context_hash);
            return Ok(summary);
        }

        let tool_results = execute_tools(&response, tools, emitter).await;
        context.push_tool_results(tool_results)?;
    }

    Err(FlickError::IterationLimit(max_iterations))
}

/// Emit usage, warnings, thinking, text, and tool call events.
fn emit_response_events(response: &ModelResponse, emitter: &mut dyn EventEmitter) {
    emitter.emit(&Event::Usage {
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
        cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
        cache_read_input_tokens: response.usage.cache_read_input_tokens,
    });

    for warning in &response.warnings {
        emitter.emit(&Event::Error {
            message: warning.message.clone(),
            code: warning.code.clone(),
            fatal: false,
        });
    }

    for thinking in &response.thinking {
        emitter.emit(&Event::Thinking { text: thinking.text.clone() });
        emitter.emit(&Event::ThinkingSignature { signature: thinking.signature.clone() });
    }

    if let Some(text) = &response.text {
        emitter.emit(&Event::Text { text: text.clone() });
    }

    for tc in &response.tool_calls {
        emitter.emit(&Event::ToolCall {
            call_id: tc.call_id.clone(),
            tool_name: tc.tool_name.clone(),
            arguments: tc.arguments.clone(),
        });
    }
}

/// Build context content blocks from the model response.
fn build_assistant_content(response: &ModelResponse) -> Vec<ContentBlock> {
    let mut content: Vec<ContentBlock> = Vec::new();

    for thinking in &response.thinking {
        content.push(ContentBlock::Thinking {
            text: thinking.text.clone(),
            signature: thinking.signature.clone(),
        });
    }

    if let Some(text) = &response.text {
        content.push(ContentBlock::Text { text: text.clone() });
    }

    for tc in &response.tool_calls {
        let input = serde_json::from_str(&tc.arguments)
            .unwrap_or_else(|_| serde_json::json!({"raw": tc.arguments}));
        content.push(ContentBlock::ToolUse {
            id: tc.call_id.clone(),
            name: tc.tool_name.clone(),
            input,
        });
    }

    content
}

/// Execute tool calls concurrently, emit results, return content blocks.
async fn execute_tools(
    response: &ModelResponse,
    tools: &ToolRegistry,
    emitter: &mut dyn EventEmitter,
) -> Vec<ContentBlock> {
    let exec_results: Vec<(bool, String)> = join_all(
        response.tool_calls.iter().map(|tc| {
            let parsed: Result<serde_json::Value, String> =
                serde_json::from_str(&tc.arguments)
                    .map_err(|e| format!("invalid tool arguments JSON: {e}"));
            async move {
                match parsed {
                    Err(msg) => (false, msg),
                    Ok(val) => match tools.execute(&tc.tool_name, &val).await {
                        Ok(out) => (true, out),
                        Err(e) => (false, e.to_string()),
                    },
                }
            }
        }),
    )
    .await;

    let mut tool_results = Vec::new();
    for (tc, (success, output)) in response.tool_calls.iter().zip(exec_results) {
        emitter.emit(&Event::ToolResult {
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

    tool_results
}

/// Compute xxh3-128 hash of serialized context. Returns `None` if
/// serialization fails (should not happen for valid Context).
fn context_hash(context: &Context) -> Option<String> {
    let bytes = serde_json::to_vec(context).ok()?;
    let hash = xxh3_128(&bytes);
    Some(format!("{hash:032x}"))
}

fn emit_done(
    emitter: &mut dyn EventEmitter,
    config: &Config,
    input_tokens: u64,
    output_tokens: u64,
    iterations: u32,
    context_hash: Option<String>,
) -> RunSummary {
    let usage = RunSummary {
        input_tokens,
        output_tokens,
        cost_usd: config.compute_cost(input_tokens, output_tokens),
        iterations,
        context_hash,
    };
    emitter.emit(&Event::Done { usage: usage.clone() });
    usage
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
