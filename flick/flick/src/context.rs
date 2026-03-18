use serde::{Deserialize, Serialize};
use std::path::Path;

/// Maximum messages in a context before push methods refuse to add more.
/// Prevents OOM from runaway context growth across resumed sessions.
const MAX_CONTEXT_MESSAGES: usize = 1024;

/// Serializable conversation history. The caller writes this between Flick
/// invocations to maintain multi-turn context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Context {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Internal field `text` maps to Anthropic wire field `thinking`.
    /// The rename happens in `messages::convert_message`, not via serde,
    /// because this type represents our internal format, not the wire format.
    Thinking {
        text: String,
        #[serde(default)]
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// Captures any content block type not yet modeled (e.g. "image").
    /// Preserves the raw JSON so round-tripping doesn't lose data.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl Context {
    pub async fn load_from_file(path: &std::path::Path) -> Result<Self, crate::error::FlickError> {
        let data = tokio::fs::read_to_string(path).await?;
        let ctx: Self =
            serde_json::from_str(&data).map_err(crate::error::FlickError::ContextParse)?;
        Self::validate_message_order(&ctx.messages)?;
        Ok(ctx)
    }

    /// Validates that the message sequence is well-formed for the API:
    /// - First message must be User (if any exist)
    /// - No two consecutive messages with the same role
    /// - `ToolResult` blocks only appear in User messages
    fn validate_message_order(messages: &[Message]) -> Result<(), crate::error::FlickError> {
        if messages.is_empty() {
            return Ok(());
        }

        if messages[0].role != Role::User {
            return Err(crate::error::FlickError::InvalidMessageOrder(
                "first message must have role \"user\"".into(),
            ));
        }

        for (i, msg) in messages.iter().enumerate() {
            if i > 0 && msg.role == messages[i - 1].role {
                return Err(crate::error::FlickError::InvalidMessageOrder(format!(
                    "consecutive {:?} messages at index {} and {}",
                    msg.role,
                    i - 1,
                    i
                )));
            }

            if msg.role != Role::User
                && msg
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                return Err(crate::error::FlickError::InvalidMessageOrder(format!(
                    "ToolResult block in non-user message at index {i}"
                )));
            }
        }

        Ok(())
    }

    pub fn push_user_text(
        &mut self,
        text: impl Into<String>,
    ) -> Result<(), crate::error::FlickError> {
        if self.messages.len() >= MAX_CONTEXT_MESSAGES {
            return Err(crate::error::FlickError::ContextOverflow(
                MAX_CONTEXT_MESSAGES,
            ));
        }
        self.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        });
        Ok(())
    }

    pub fn push_assistant(
        &mut self,
        content: Vec<ContentBlock>,
    ) -> Result<(), crate::error::FlickError> {
        if self.messages.len() >= MAX_CONTEXT_MESSAGES {
            return Err(crate::error::FlickError::ContextOverflow(
                MAX_CONTEXT_MESSAGES,
            ));
        }
        if content.is_empty() {
            return Err(crate::error::FlickError::InvalidAssistantContent(
                "push_assistant called with empty content".into(),
            ));
        }
        self.messages.push(Message {
            role: Role::Assistant,
            content,
        });
        Ok(())
    }

    pub fn push_tool_results(
        &mut self,
        results: Vec<ContentBlock>,
    ) -> Result<(), crate::error::FlickError> {
        if self.messages.len() >= MAX_CONTEXT_MESSAGES {
            return Err(crate::error::FlickError::ContextOverflow(
                MAX_CONTEXT_MESSAGES,
            ));
        }
        if results.is_empty() {
            return Err(crate::error::FlickError::InvalidToolResults(
                "push_tool_results called with empty results".into(),
            ));
        }
        if !results
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        {
            return Err(crate::error::FlickError::InvalidToolResults(
                "push_tool_results called with non-ToolResult blocks".into(),
            ));
        }
        self.messages.push(Message {
            role: Role::User,
            content: results,
        });
        Ok(())
    }
}

/// Reads a JSON file of tool results into `ContentBlock::ToolResult` variants.
///
/// The file format matches the `--tool-results` CLI input described in the
/// monadic tools spec.
pub async fn load_tool_results(path: &Path) -> Result<Vec<ContentBlock>, crate::error::FlickError> {
    let data = tokio::fs::read_to_string(path).await?;

    // Deserialize into raw JSON values first so we can produce specific
    // validation errors instead of opaque serde messages.
    let entries: Vec<serde_json::Value> = serde_json::from_str(&data)
        .map_err(|e| crate::error::FlickError::ToolResultParse(e.to_string()))?;

    if entries.is_empty() {
        return Err(crate::error::FlickError::ToolResultParse(
            "tool results array is empty".into(),
        ));
    }

    let mut results = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let tool_use_id = entry
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error::FlickError::ToolResultParse(format!(
                    "entry {i}: missing or non-string \"tool_use_id\""
                ))
            })?;

        let content = entry
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error::FlickError::ToolResultParse(format!(
                    "entry {i}: missing or non-string \"content\""
                ))
            })?;

        let is_error = entry
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        results.push(ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_owned(),
            content: content.to_owned(),
            is_error,
        });
    }

    Ok(results)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn push_user_text_adds_message() {
        let mut ctx = Context::default();
        ctx.push_user_text("hello").unwrap();
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, Role::User);
        match &ctx.messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn push_assistant_adds_message() {
        let mut ctx = Context::default();
        ctx.push_assistant(vec![ContentBlock::Text {
            text: "reply".into(),
        }])
        .unwrap();
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, Role::Assistant);
    }

    #[test]
    fn push_tool_results_adds_user_message() {
        let mut ctx = Context::default();
        ctx.push_tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "id1".into(),
            content: "output".into(),
            is_error: false,
        }])
        .unwrap();
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, Role::User);
    }

    #[test]
    fn serde_round_trip() {
        let mut ctx = Context::default();
        ctx.push_user_text("test").unwrap();
        ctx.push_assistant(vec![ContentBlock::Text {
            text: "response".into(),
        }])
        .unwrap();
        let json = serde_json::to_string(&ctx).expect("serialize");
        let restored: Context = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.messages.len(), 2);
        assert_eq!(restored.messages[0].role, Role::User);
        assert_eq!(restored.messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn load_from_file_with_temp_file() {
        use std::io::Write;

        let mut ctx = Context::default();
        ctx.push_user_text("saved").unwrap();
        let json = serde_json::to_string(&ctx).expect("serialize");

        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(json.as_bytes()).expect("write temp file");

        let loaded = Context::load_from_file(f.path()).await.expect("load");
        assert_eq!(loaded.messages.len(), 1);
        match &loaded.messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "saved"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn serde_round_trip_with_tool_use() {
        let mut ctx = Context::default();
        ctx.push_assistant(vec![
            ContentBlock::Text {
                text: "calling tool".into(),
            },
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test"}),
            },
        ])
        .unwrap();
        ctx.push_tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: "file contents".into(),
            is_error: false,
        }])
        .unwrap();
        let json = serde_json::to_string(&ctx).expect("serialize");
        let restored: Context = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.messages.len(), 2);
        assert!(
            matches!(&restored.messages[0].content[1], ContentBlock::ToolUse { name, .. } if name == "read_file")
        );
        assert!(
            matches!(&restored.messages[1].content[0], ContentBlock::ToolResult { content, is_error, .. } if content == "file contents" && !is_error)
        );
    }

    #[test]
    fn serde_round_trip_thinking_empty_signature() {
        let mut ctx = Context::default();
        ctx.push_assistant(vec![ContentBlock::Thinking {
            text: "reasoning".into(),
            signature: String::new(),
        }])
        .unwrap();
        let json = serde_json::to_string(&ctx).expect("serialize");
        let restored: Context = serde_json::from_str(&json).expect("deserialize");
        match &restored.messages[0].content[0] {
            ContentBlock::Thinking { text, signature } => {
                assert_eq!(text, "reasoning");
                assert!(signature.is_empty());
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_from_file_nonexistent_path() {
        let result =
            Context::load_from_file(std::path::Path::new("/nonexistent/context.json")).await;
        assert!(matches!(result, Err(crate::error::FlickError::Io(_))));
    }

    #[tokio::test]
    async fn load_from_file_invalid_json() {
        use std::io::Write;

        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(b"not json").expect("write");
        let result = Context::load_from_file(f.path()).await;
        assert!(matches!(
            result,
            Err(crate::error::FlickError::ContextParse(_))
        ));
    }

    #[test]
    fn thinking_block_missing_signature_defaults_empty() {
        let json = r#"{"type":"thinking","text":"reasoning here"}"#;
        let block: ContentBlock = serde_json::from_str(json).expect("deserialize");
        match block {
            ContentBlock::Thinking { text, signature } => {
                assert_eq!(text, "reasoning here");
                assert!(signature.is_empty());
            }
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_is_error_round_trip() {
        let original = ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: "not found".into(),
            is_error: true,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: ContentBlock = serde_json::from_str(&json).expect("deserialize");
        match restored {
            ContentBlock::ToolResult { is_error, .. } => assert!(is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_from_file_valid_json_wrong_shape() {
        use std::io::Write;

        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(br#"{"not_messages": []}"#).expect("write");
        let result = Context::load_from_file(f.path()).await;
        assert!(matches!(
            result,
            Err(crate::error::FlickError::ContextParse(_))
        ));
    }

    #[test]
    fn push_rejects_at_max_context_messages() {
        let mut ctx = Context::default();
        // Fill to MAX_CONTEXT_MESSAGES - 1 (1023 messages)
        for i in 0..1023 {
            if i % 2 == 0 {
                ctx.push_user_text(format!("msg {i}")).unwrap();
            } else {
                ctx.push_assistant(vec![ContentBlock::Text {
                    text: format!("msg {i}"),
                }])
                .unwrap();
            }
        }
        assert_eq!(ctx.messages.len(), 1023);
        // Push #1024 succeeds (len goes from 1023 to 1024)
        ctx.push_user_text("last valid").unwrap();
        assert_eq!(ctx.messages.len(), 1024);
        // Push #1025 is rejected (len == MAX_CONTEXT_MESSAGES)
        let result = ctx.push_user_text("overflow");
        assert!(matches!(
            result,
            Err(crate::error::FlickError::ContextOverflow(1024))
        ));
    }

    #[test]
    fn push_tool_results_rejects_empty_vec() {
        let mut ctx = Context::default();
        let result = ctx.push_tool_results(vec![]);
        assert!(matches!(
            result,
            Err(crate::error::FlickError::InvalidToolResults(_))
        ));
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn push_tool_results_rejects_non_tool_result_blocks() {
        let mut ctx = Context::default();
        let result = ctx.push_tool_results(vec![ContentBlock::Text {
            text: "hello".into(),
        }]);
        assert!(matches!(
            result,
            Err(crate::error::FlickError::InvalidToolResults(_))
        ));
        assert!(result.unwrap_err().to_string().contains("non-ToolResult"));
    }

    // --- load_tool_results tests ---

    fn write_temp_file(content: &[u8]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(content).expect("write temp file");
        f
    }

    #[tokio::test]
    async fn load_tool_results_valid_input() {
        let json = br#"[
            {"tool_use_id": "tc_1", "content": "file contents", "is_error": false},
            {"tool_use_id": "tc_2", "content": "command failed", "is_error": true}
        ]"#;
        let f = write_temp_file(json);
        let results = load_tool_results(f.path()).await.unwrap();
        assert_eq!(results.len(), 2);

        match &results[0] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "tc_1");
                assert_eq!(content, "file contents");
                assert!(!is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        match &results[1] {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "tc_2");
                assert_eq!(content, "command failed");
                assert!(*is_error);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_tool_results_is_error_defaults_false() {
        let json = br#"[{"tool_use_id": "tc_1", "content": "ok"}]"#;
        let f = write_temp_file(json);
        let results = load_tool_results(f.path()).await.unwrap();
        match &results[0] {
            ContentBlock::ToolResult { is_error, .. } => assert!(!is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_tool_results_missing_tool_use_id() {
        let json = br#"[{"content": "data", "is_error": false}]"#;
        let f = write_temp_file(json);
        let err = load_tool_results(f.path()).await.unwrap_err();
        assert!(matches!(err, crate::error::FlickError::ToolResultParse(_)));
        assert!(err.to_string().contains("tool_use_id"));
    }

    #[tokio::test]
    async fn load_tool_results_missing_content() {
        let json = br#"[{"tool_use_id": "tc_1", "is_error": false}]"#;
        let f = write_temp_file(json);
        let err = load_tool_results(f.path()).await.unwrap_err();
        assert!(matches!(err, crate::error::FlickError::ToolResultParse(_)));
        assert!(err.to_string().contains("content"));
    }

    #[tokio::test]
    async fn load_tool_results_empty_array() {
        let json = b"[]";
        let f = write_temp_file(json);
        let err = load_tool_results(f.path()).await.unwrap_err();
        assert!(matches!(err, crate::error::FlickError::ToolResultParse(_)));
        assert!(err.to_string().contains("empty"));
    }

    #[tokio::test]
    async fn load_tool_results_malformed_json() {
        let f = write_temp_file(b"not json at all");
        let err = load_tool_results(f.path()).await.unwrap_err();
        assert!(matches!(err, crate::error::FlickError::ToolResultParse(_)));
    }

    #[tokio::test]
    async fn load_tool_results_nonexistent_file() {
        let err = load_tool_results(Path::new("/nonexistent/results.json"))
            .await
            .unwrap_err();
        assert!(matches!(err, crate::error::FlickError::Io(_)));
    }

    #[tokio::test]
    async fn load_tool_results_not_an_array() {
        let json = br#"{"tool_use_id": "tc_1", "content": "data"}"#;
        let f = write_temp_file(json);
        let err = load_tool_results(f.path()).await.unwrap_err();
        // A JSON object instead of array fails at the Vec<Value> parse step
        assert!(matches!(err, crate::error::FlickError::ToolResultParse(_)));
    }

    #[tokio::test]
    async fn load_tool_results_non_boolean_is_error_defaults_false() {
        let json = br#"[{"tool_use_id": "tc_1", "content": "ok", "is_error": "true"}]"#;
        let f = write_temp_file(json);
        let results = load_tool_results(f.path()).await.unwrap();
        match &results[0] {
            ContentBlock::ToolResult { is_error, .. } => assert!(!is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_tool_results_integrates_with_push() {
        let json = br#"[{"tool_use_id": "tc_1", "content": "output", "is_error": false}]"#;
        let f = write_temp_file(json);
        let results = load_tool_results(f.path()).await.unwrap();
        let mut ctx = Context::default();
        ctx.push_tool_results(results).unwrap();
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, Role::User);
        assert!(matches!(
            &ctx.messages[0].content[0],
            ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tc_1"
        ));
    }

    // --- L7: Unknown content block variant ---

    #[test]
    fn unknown_content_block_deserializes() {
        let json = r#"{"type":"image","source":{"url":"https://example.com/img.png"}}"#;
        let block: ContentBlock = serde_json::from_str(json).expect("deserialize");
        match &block {
            ContentBlock::Unknown(v) => {
                assert_eq!(v["type"], "image");
                assert_eq!(v["source"]["url"], "https://example.com/img.png");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn unknown_content_block_round_trips() {
        let json = r#"{"type":"image","source":{"url":"https://example.com/img.png"}}"#;
        let block: ContentBlock = serde_json::from_str(json).expect("deserialize");
        let serialized = serde_json::to_string(&block).expect("serialize");
        let value: serde_json::Value = serde_json::from_str(&serialized).expect("re-parse");
        assert_eq!(value["type"], "image");
    }

    #[test]
    fn context_with_unknown_block_round_trips() {
        let json = r#"{"messages":[{"role":"user","content":[{"type":"image","url":"x"}]}]}"#;
        let ctx: Context = serde_json::from_str(json).expect("deserialize");
        assert!(matches!(
            &ctx.messages[0].content[0],
            ContentBlock::Unknown(_)
        ));
        let reserialized = serde_json::to_string(&ctx).expect("serialize");
        let restored: Context = serde_json::from_str(&reserialized).expect("deserialize again");
        assert!(matches!(
            &restored.messages[0].content[0],
            ContentBlock::Unknown(_)
        ));
    }

    // --- T24: push_assistant rejects empty content ---

    #[test]
    fn push_assistant_rejects_empty_content() {
        let mut ctx = Context::default();
        let result = ctx.push_assistant(vec![]);
        assert!(matches!(
            result,
            Err(crate::error::FlickError::InvalidAssistantContent(_))
        ));
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    // --- T75: load_from_file validates message ordering ---

    #[tokio::test]
    async fn load_rejects_assistant_first() {
        let json = r#"{"messages":[{"role":"assistant","content":[{"type":"text","text":"hi"}]}]}"#;
        let f = write_temp_file(json.as_bytes());
        let err = Context::load_from_file(f.path()).await.unwrap_err();
        assert!(matches!(
            err,
            crate::error::FlickError::InvalidMessageOrder(_)
        ));
        assert!(err.to_string().contains("first message"));
    }

    #[tokio::test]
    async fn load_rejects_consecutive_same_role() {
        let json = r#"{"messages":[
            {"role":"user","content":[{"type":"text","text":"a"}]},
            {"role":"user","content":[{"type":"text","text":"b"}]}
        ]}"#;
        let f = write_temp_file(json.as_bytes());
        let err = Context::load_from_file(f.path()).await.unwrap_err();
        assert!(matches!(
            err,
            crate::error::FlickError::InvalidMessageOrder(_)
        ));
        assert!(err.to_string().contains("consecutive"));
    }

    #[tokio::test]
    async fn load_rejects_tool_result_in_assistant_message() {
        let json = r#"{"messages":[
            {"role":"user","content":[{"type":"text","text":"q"}]},
            {"role":"assistant","content":[{"type":"tool_result","tool_use_id":"x","content":"y"}]}
        ]}"#;
        let f = write_temp_file(json.as_bytes());
        let err = Context::load_from_file(f.path()).await.unwrap_err();
        assert!(matches!(
            err,
            crate::error::FlickError::InvalidMessageOrder(_)
        ));
        assert!(err.to_string().contains("ToolResult"));
    }

    #[tokio::test]
    async fn load_accepts_valid_alternating_messages() {
        let json = r#"{"messages":[
            {"role":"user","content":[{"type":"text","text":"q"}]},
            {"role":"assistant","content":[{"type":"text","text":"a"}]},
            {"role":"user","content":[{"type":"text","text":"q2"}]}
        ]}"#;
        let f = write_temp_file(json.as_bytes());
        let ctx = Context::load_from_file(f.path()).await.unwrap();
        assert_eq!(ctx.messages.len(), 3);
    }

    #[tokio::test]
    async fn load_accepts_empty_messages() {
        let json = r#"{"messages":[]}"#;
        let f = write_temp_file(json.as_bytes());
        let ctx = Context::load_from_file(f.path()).await.unwrap();
        assert!(ctx.messages.is_empty());
    }

    // --- T78: missing content key defaults to empty vec ---

    #[test]
    fn message_missing_content_defaults_empty() {
        let json = r#"{"role":"user"}"#;
        let msg: Message = serde_json::from_str(json).expect("deserialize");
        assert!(msg.content.is_empty());
    }

    // --- Known types must not fall into Unknown ---

    #[test]
    fn deserialize_text_block_is_not_unknown() {
        let json = r#"{"type":"text","text":"hi"}"#;
        let block: ContentBlock = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(block, ContentBlock::Text { ref text } if text == "hi"),
            "expected Text, got {block:?}"
        );
    }

    #[test]
    fn deserialize_tool_use_block_is_not_unknown() {
        let json = r#"{"type":"tool_use","id":"c1","name":"run","input":{}}"#;
        let block: ContentBlock = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(block, ContentBlock::ToolUse { ref name, .. } if name == "run"),
            "expected ToolUse, got {block:?}"
        );
    }

    #[test]
    fn deserialize_tool_result_block_is_not_unknown() {
        let json = r#"{"type":"tool_result","tool_use_id":"c1","content":"ok"}"#;
        let block: ContentBlock = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(block, ContentBlock::ToolResult { ref tool_use_id, .. } if tool_use_id == "c1"),
            "expected ToolResult, got {block:?}"
        );
    }

    // --- push_assistant happy path with prior user message ---

    #[test]
    fn push_assistant_after_user_appends_correctly() {
        let mut ctx = Context::default();
        ctx.push_user_text("question").unwrap();
        ctx.push_assistant(vec![
            ContentBlock::Text {
                text: "answer".into(),
            },
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "search".into(),
                input: serde_json::json!({"q": "rust"}),
            },
        ])
        .unwrap();
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[1].role, Role::Assistant);
        assert_eq!(ctx.messages[1].content.len(), 2);
        match &ctx.messages[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "answer"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert!(matches!(
            &ctx.messages[1].content[1],
            ContentBlock::ToolUse { name, .. } if name == "search"
        ));
    }
}
