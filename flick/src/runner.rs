use crate::config::Config;
use crate::context::{ContentBlock, Context};
use crate::error::FlickError;
use crate::provider::{DynProvider, ModelResponse, RequestParams, ToolDefinition};
use crate::result::{FlickResult, ResultStatus, UsageSummary};

/// Make a single model call and return the result.
///
/// Does not execute tools. If the model returns tool-use blocks, the result
/// status is `ToolCallsPending` and the caller is responsible for executing
/// tools, appending results to the context, and re-invoking.
pub async fn run(
    config: &Config,
    provider: &dyn DynProvider,
    context: &mut Context,
) -> Result<FlickResult, FlickError> {
    let tool_defs: Vec<ToolDefinition> = config
        .tools()
        .iter()
        .map(super::config::ToolConfig::to_definition)
        .collect();

    let params = build_params(config, &context.messages, &tool_defs);
    let response = provider.call_boxed(params).await?;

    let blocks = build_content(&response)?;

    if !blocks.is_empty() {
        context.push_assistant(blocks.clone())?;
    }

    let has_tool_use = blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let status = if has_tool_use {
        ResultStatus::ToolCallsPending
    } else {
        ResultStatus::Complete
    };

    let cost_usd = config.compute_cost(
        response.usage.input_tokens,
        response.usage.output_tokens,
    );

    Ok(FlickResult {
        status,
        content: blocks,
        usage: Some(UsageSummary {
            input_tokens: response.usage.input_tokens,
            output_tokens: response.usage.output_tokens,
            cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
            cache_read_input_tokens: response.usage.cache_read_input_tokens,
            cost_usd,
        }),
        context_hash: None,
        error: None,
    })
}

/// Build content blocks from the model response.
fn build_content(response: &ModelResponse) -> Result<Vec<ContentBlock>, FlickError> {
    let mut content = Vec::new();

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
        let input: serde_json::Value = serde_json::from_str(&tc.arguments)
            .map_err(|e| FlickError::Provider(crate::error::ProviderError::ResponseParse(format!(
                "malformed tool call arguments for '{}': {e}",
                tc.tool_name
            ))))?;
        content.push(ContentBlock::ToolUse {
            id: tc.call_id.clone(),
            name: tc.tool_name.clone(),
            input,
        });
    }

    Ok(content)
}

/// Build provider request parameters from config and context.
///
/// Public because `--dry-run` in main.rs calls this directly.
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
