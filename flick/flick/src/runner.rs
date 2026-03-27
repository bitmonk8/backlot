use std::time::Instant;

use crate::ApiKind;
use crate::config::RequestConfig;
use crate::context::{ContentBlock, Context};
use crate::error::FlickError;
use crate::model_registry::ModelInfo;
use crate::provider::{DynProvider, ModelResponse, RequestParams, ToolDefinition};
use crate::result::{FlickResult, ResultStatus, Timing, UsageSummary};
use crate::structured_output::{check_required_fields, strip_fences_from_blocks};

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
    let start = Instant::now();
    let response = provider.call_boxed(params).await?;
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    let mut blocks = build_content(&response)?;

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
        return run_second_step(
            config, model_info, provider, context, &response, &blocks, elapsed_ms,
        )
        .await;
    }

    // Strip fences and validate against schema when present.
    if status == ResultStatus::Complete {
        if let Some(output_schema) = config.output_schema() {
            validate_and_update_context(&mut blocks, context, &output_schema.schema, None)?;
        }
    }

    let cost_usd = model_info.compute_cost(
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
        timing: Some(Timing {
            api_latency_ms: elapsed_ms,
        }),
        context_hash: None,
        error: None,
    })
}

/// Second step of the two-step structured output path. Pops the first
/// assistant message, calls the provider with the schema, and restores
/// the saved message on any error.
async fn run_second_step(
    config: &RequestConfig,
    model_info: &ModelInfo,
    provider: &dyn DynProvider,
    context: &mut Context,
    first_response: &ModelResponse,
    first_blocks: &[ContentBlock],
    first_elapsed_ms: u64,
) -> Result<FlickResult, FlickError> {
    let saved = if first_blocks.is_empty() {
        None
    } else {
        context.messages.pop()
    };

    let result: Result<(ModelResponse, Vec<ContentBlock>, u64), FlickError> = async {
        let params2 = build_params(config, model_info, &context.messages, &[]);
        let start2 = Instant::now();
        let response2 = provider.call_boxed(params2).await?;
        let elapsed2_ms = u64::try_from(start2.elapsed().as_millis()).unwrap_or(u64::MAX);
        let blocks2 = build_content(&response2)?;
        if !blocks2.is_empty() {
            context.push_assistant(blocks2.clone())?;
        }
        Ok((response2, blocks2, elapsed2_ms))
    }
    .await;

    let (response2, mut blocks2, second_elapsed_ms) = match result {
        Ok(triple) => triple,
        Err(e) => {
            if let Some(msg) = saved {
                context.messages.push(msg);
            }
            return Err(e);
        }
    };

    // Strip fences and validate against schema.
    if let Some(output_schema) = config.output_schema() {
        validate_and_update_context(&mut blocks2, context, &output_schema.schema, saved)?;
    }

    let total_input = first_response.usage.input_tokens + response2.usage.input_tokens;
    let total_output = first_response.usage.output_tokens + response2.usage.output_tokens;
    let total_cache_creation = first_response.usage.cache_creation_input_tokens
        + response2.usage.cache_creation_input_tokens;
    let total_cache_read =
        first_response.usage.cache_read_input_tokens + response2.usage.cache_read_input_tokens;
    let cost_usd = model_info.compute_cost(
        total_input,
        total_output,
        total_cache_creation,
        total_cache_read,
    );

    let total_latency_ms = first_elapsed_ms.saturating_add(second_elapsed_ms);

    Ok(FlickResult {
        status: ResultStatus::Complete,
        content: blocks2,
        usage: Some(UsageSummary {
            input_tokens: total_input,
            output_tokens: total_output,
            cache_creation_input_tokens: total_cache_creation,
            cache_read_input_tokens: total_cache_read,
            cost_usd,
        }),
        timing: Some(Timing {
            api_latency_ms: total_latency_ms,
        }),
        context_hash: None,
        error: None,
    })
}

/// Strip markdown fences, check required fields, and replace the context
/// assistant message with cleaned content.  On validation failure, rolls back:
/// pops the stale assistant message and restores `saved` (if provided).
fn validate_and_update_context(
    blocks: &mut [ContentBlock],
    context: &mut Context,
    schema: &serde_json::Value,
    saved: Option<crate::context::Message>,
) -> Result<(), FlickError> {
    strip_fences_from_blocks(blocks);
    if let Err(e) = check_required_fields(blocks, schema) {
        if !blocks.is_empty() {
            context.messages.pop();
        }
        if let Some(msg) = saved {
            context.messages.push(msg);
        }
        return Err(e);
    }
    // Replace context message with cleaned (fence-stripped) content.
    if !blocks.is_empty() {
        context.messages.pop();
        context.push_assistant(blocks.to_vec())?;
    }
    Ok(())
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::ApiKind;
    use crate::config::RequestConfig;
    use crate::context::Context;
    use crate::model_registry::ModelInfo;
    use crate::provider::{ModelResponse, UsageResponse};
    use crate::test_support::{MultiShotProvider, SingleShotProvider};

    fn test_model_info() -> ModelInfo {
        ModelInfo {
            provider: "test".into(),
            name: "mock-model".into(),
            max_tokens: Some(1024),
            input_per_million: None,
            output_per_million: None,
            cache_creation_per_million: None,
            cache_read_per_million: None,
        }
    }

    fn schema_config() -> RequestConfig {
        RequestConfig::parse_yaml(
            "model: test\noutput_schema:\n  schema:\n    type: object\n    required: [answer]\n    properties:\n      answer:\n        type: string\n",
        )
        .unwrap()
    }

    fn schema_with_tools_config() -> RequestConfig {
        RequestConfig::parse_yaml(
            "model: test\noutput_schema:\n  schema:\n    type: object\n    required: [answer]\n    properties:\n      answer:\n        type: string\ntools:\n  - name: read_file\n    description: Read a file\n    parameters:\n      type: object\n      properties:\n        path:\n          type: string\n      required: [path]\n",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn no_validation_when_no_schema() {
        let provider = SingleShotProvider::with_text("not json at all");
        let config = RequestConfig::parse_yaml("model: test\n").unwrap();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let result = run(
            &config,
            &mi,
            ApiKind::Messages,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(result.status, ResultStatus::Complete);
        match &result.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "not json at all"),
            _ => panic!("expected Text block"),
        }
    }

    #[tokio::test]
    async fn single_step_valid_json_cleaned_and_passes() {
        let provider = SingleShotProvider::with_text("```json\n{\"answer\": \"hi\"}\n```");
        let config = schema_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let result = run(
            &config,
            &mi,
            ApiKind::Messages,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(result.status, ResultStatus::Complete);
        match &result.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "{\"answer\": \"hi\"}"),
            _ => panic!("expected Text block"),
        }
        // Context also has cleaned content
        let assistant_msg = &context.messages[1];
        match &assistant_msg.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "{\"answer\": \"hi\"}"),
            _ => panic!("expected Text block in context"),
        }
    }

    #[tokio::test]
    async fn single_step_missing_required_field_errors() {
        let provider = SingleShotProvider::with_text(r#"{"wrong": "field"}"#);
        let config = schema_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let err = run(
            &config,
            &mi,
            ApiKind::Messages,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(ref msg) if msg.contains("answer")));
        // Stale assistant message popped
        assert_eq!(context.messages.len(), 1);
    }

    #[tokio::test]
    async fn two_step_valid_json_passes() {
        let provider = MultiShotProvider::new(vec![
            ModelResponse {
                text: Some("thinking...".into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            },
            ModelResponse {
                text: Some(r#"{"answer": "done"}"#.into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse {
                    input_tokens: 200,
                    output_tokens: 30,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            },
        ]);
        let config = schema_with_tools_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let result = run(
            &config,
            &mi,
            ApiKind::ChatCompletions,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(result.status, ResultStatus::Complete);
        let usage = result.usage.unwrap();
        assert_eq!(usage.input_tokens, 300);
        assert_eq!(usage.output_tokens, 80);
        match &result.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, r#"{"answer": "done"}"#),
            _ => panic!("expected Text block"),
        }
        // Context: user + assistant (second-step result replaces first-step)
        assert_eq!(context.messages.len(), 2);
        match &context.messages[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, r#"{"answer": "done"}"#),
            _ => panic!("expected Text block in context"),
        }
    }

    #[tokio::test]
    async fn two_step_missing_required_field_restores_context() {
        let provider = MultiShotProvider::new(vec![
            ModelResponse {
                text: Some("first step output".into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse::default(),
            },
            ModelResponse {
                text: Some(r#"{"wrong": "field"}"#.into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse::default(),
            },
        ]);
        let config = schema_with_tools_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let err = run(
            &config,
            &mi,
            ApiKind::ChatCompletions,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(ref msg) if msg.contains("answer")));
        // First-step assistant message restored after second-step validation failure
        assert_eq!(context.messages.len(), 2);
        match &context.messages[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "first step output"),
            _ => panic!("expected first-step Text block restored"),
        }
    }

    #[tokio::test]
    async fn two_step_fences_stripped() {
        let provider = MultiShotProvider::new(vec![
            ModelResponse {
                text: Some("ok".into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse::default(),
            },
            ModelResponse {
                text: Some("```json\n{\"answer\": \"hi\"}\n```".into()),
                thinking: Vec::new(),
                tool_calls: Vec::new(),
                usage: UsageResponse::default(),
            },
        ]);
        let config = schema_with_tools_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let result = run(
            &config,
            &mi,
            ApiKind::ChatCompletions,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(result.status, ResultStatus::Complete);
        match &result.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "{\"answer\": \"hi\"}"),
            _ => panic!("expected Text block"),
        }
    }

    // --- #18: ResponseNotJson error propagates through runner ---

    #[tokio::test]
    async fn single_step_non_json_with_schema_errors() {
        let provider = SingleShotProvider::with_text("plain text, not json");
        let config = schema_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let err = run(
            &config,
            &mi,
            ApiKind::Messages,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FlickError::ResponseNotJson(_)));
        // Stale assistant message popped
        assert_eq!(context.messages.len(), 1);
    }

    // --- #19: second provider call failure restores context ---

    #[tokio::test]
    async fn two_step_second_call_failure_restores_context() {
        // First call succeeds, second call fails (provider exhausted).
        let provider = MultiShotProvider::new(vec![ModelResponse {
            text: Some("first step".into()),
            thinking: Vec::new(),
            tool_calls: Vec::new(),
            usage: UsageResponse::default(),
        }]);
        let config = schema_with_tools_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let err = run(
            &config,
            &mi,
            ApiKind::ChatCompletions,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, FlickError::Provider(_)));
        // First-step assistant message restored after second-call failure
        assert_eq!(context.messages.len(), 2);
        match &context.messages[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "first step"),
            _ => panic!("expected first-step Text restored"),
        }
    }

    // --- #20: schema present + tool calls → validation skipped ---

    #[tokio::test]
    async fn schema_with_tool_calls_skips_validation() {
        use crate::provider::ToolCallResponse;
        let provider = SingleShotProvider::with_tool_calls(vec![ToolCallResponse {
            call_id: "tc_1".into(),
            tool_name: "read_file".into(),
            arguments: r#"{"path":"/tmp"}"#.into(),
        }]);
        let config = schema_config();
        let mi = test_model_info();
        let mut context = Context::default();
        context.push_user_text("test").unwrap();

        let result = run(
            &config,
            &mi,
            ApiKind::Messages,
            provider.as_ref(),
            &mut context,
        )
        .await
        .unwrap();
        // Tool calls returned — validation is skipped even though schema is present.
        assert_eq!(result.status, ResultStatus::ToolCallsPending);
    }
}
