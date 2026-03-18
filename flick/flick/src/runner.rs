use crate::ApiKind;
use crate::config::RequestConfig;
use crate::context::{ContentBlock, Context};
use crate::error::FlickError;
use crate::model_registry::ModelInfo;
use crate::provider::{DynProvider, ModelResponse, RequestParams, ToolDefinition};
use crate::result::{FlickResult, ResultStatus, UsageSummary};

/// Make a single model call and return the result.
///
/// Does not execute tools. If the model returns tool-use blocks, the result
/// status is `ToolCallsPending` and the caller is responsible for executing
/// tools, appending results to the context, and re-invoking.
///
/// When the config specifies both tools and `output_schema` with a Chat
/// Completions provider (which doesn't support both simultaneously), the
/// runner transparently performs a two-step call: first with tools (no
/// schema), then — if the model completes without tool calls — a second
/// call with the schema (no tools). Usage from both calls is summed.
pub async fn run(
    config: &RequestConfig,
    model_info: &ModelInfo,
    api_kind: ApiKind,
    provider: &dyn DynProvider,
    context: &mut Context,
) -> Result<FlickResult, FlickError> {
    let tool_defs: Vec<ToolDefinition> = config
        .tools()
        .iter()
        .map(super::config::ToolConfig::to_definition)
        .collect();

    let has_schema = config.output_schema().is_some();
    let has_tools = !tool_defs.is_empty();
    let is_chat_completions = api_kind == ApiKind::ChatCompletions;
    let needs_two_step = has_tools && has_schema && is_chat_completions;

    // First call: if two-step, omit schema so the API accepts tools.
    let mut params = build_params(config, model_info, &context.messages, &tool_defs);
    if needs_two_step {
        params.output_schema = None;
    }
    let response = provider.call_boxed(params).await?;

    let blocks = build_content(&response)?;

    if !blocks.is_empty() {
        context.push_assistant(blocks.clone())?;
    }

    let has_tool_use = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let status = if has_tool_use {
        ResultStatus::ToolCallsPending
    } else {
        ResultStatus::Complete
    };

    // Two-step: if the first call completed (no tool calls), make a second
    // call with schema and no tools to get structured output.
    if needs_two_step && status == ResultStatus::Complete {
        if !blocks.is_empty() {
            context.messages.pop();
        }

        let empty_tools: Vec<ToolDefinition> = Vec::new();
        let params2 = build_params(config, model_info, &context.messages, &empty_tools);
        let response2 = provider.call_boxed(params2).await?;
        let blocks2 = build_content(&response2)?;

        if !blocks2.is_empty() {
            context.push_assistant(blocks2.clone())?;
        }

        let total_input = response.usage.input_tokens + response2.usage.input_tokens;
        let total_output = response.usage.output_tokens + response2.usage.output_tokens;
        let total_cache_creation = response.usage.cache_creation_input_tokens
            + response2.usage.cache_creation_input_tokens;
        let total_cache_read =
            response.usage.cache_read_input_tokens + response2.usage.cache_read_input_tokens;
        let cost_usd = config.compute_cost(
            model_info,
            total_input,
            total_output,
            total_cache_creation,
            total_cache_read,
        );

        return Ok(FlickResult {
            status,
            content: blocks2,
            usage: Some(UsageSummary {
                input_tokens: total_input,
                output_tokens: total_output,
                cache_creation_input_tokens: total_cache_creation,
                cache_read_input_tokens: total_cache_read,
                cost_usd,
            }),
            context_hash: None,
            error: None,
        });
    }

    let cost_usd = config.compute_cost(
        model_info,
        response.usage.input_tokens,
        response.usage.output_tokens,
        response.usage.cache_creation_input_tokens,
        response.usage.cache_read_input_tokens,
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
        let input: serde_json::Value = serde_json::from_str(&tc.arguments).map_err(|e| {
            FlickError::Provider(crate::error::ProviderError::ResponseParse(format!(
                "malformed tool call arguments for '{}': {e}",
                tc.tool_name
            )))
        })?;
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
    config: &'a RequestConfig,
    model_info: &'a ModelInfo,
    messages: &'a [crate::context::Message],
    tool_defs: &'a [ToolDefinition],
) -> RequestParams<'a> {
    RequestParams {
        model: &model_info.name,
        max_tokens: model_info.max_tokens,
        // Strip temperature when reasoning is active (provider-agnostic)
        temperature: if config.reasoning().is_some() {
            None
        } else {
            config.temperature()
        },
        system_prompt: config.system_prompt(),
        messages,
        tools: tool_defs,
        tool_choice: config
            .tool_choice()
            .map(super::config::ToolChoiceConfig::to_tool_choice),
        reasoning: config.reasoning().map(|r| r.level),
        output_schema: config.output_schema().map(|o| &o.schema),
    }
}
