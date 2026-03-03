use serde::{Deserialize, Serialize};

/// Maximum messages in a context before push methods refuse to add more.
/// Prevents OOM from malicious or runaway context growth. The agent loop
/// runs at most 25 iterations × 2 messages each = 50 messages per session,
/// plus the initial user message and any loaded history.
const MAX_CONTEXT_MESSAGES: usize = 1024;

/// Serializable conversation history. Epic writes this between Flick
/// invocations to maintain multi-turn context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Context {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
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
}

impl Context {
    pub async fn load_from_file(path: &std::path::Path) -> Result<Self, crate::error::FlickError> {
        let data = tokio::fs::read_to_string(path).await?;
        let ctx: Self = serde_json::from_str(&data)?;
        Ok(ctx)
    }

    pub fn push_user_text(&mut self, text: impl Into<String>) -> Result<(), crate::error::FlickError> {
        if self.messages.len() >= MAX_CONTEXT_MESSAGES {
            return Err(crate::error::FlickError::ContextOverflow(MAX_CONTEXT_MESSAGES));
        }
        self.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        });
        Ok(())
    }

    pub fn push_assistant(&mut self, content: Vec<ContentBlock>) -> Result<(), crate::error::FlickError> {
        if self.messages.len() >= MAX_CONTEXT_MESSAGES {
            return Err(crate::error::FlickError::ContextOverflow(MAX_CONTEXT_MESSAGES));
        }
        self.messages.push(Message {
            role: Role::Assistant,
            content,
        });
        Ok(())
    }

    pub fn push_tool_results(&mut self, results: Vec<ContentBlock>) -> Result<(), crate::error::FlickError> {
        if self.messages.len() >= MAX_CONTEXT_MESSAGES {
            return Err(crate::error::FlickError::ContextOverflow(MAX_CONTEXT_MESSAGES));
        }
        if results.is_empty() {
            return Err(crate::error::FlickError::Tool(
                crate::error::ToolError::ExecutionFailed(
                    "push_tool_results called with empty results".into(),
                ),
            ));
        }
        if !results.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })) {
            return Err(crate::error::FlickError::Tool(
                crate::error::ToolError::ExecutionFailed(
                    "push_tool_results called with non-ToolResult blocks".into(),
                ),
            ));
        }
        self.messages.push(Message {
            role: Role::User,
            content: results,
        });
        Ok(())
    }
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
        }]).unwrap();
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
        }]).unwrap();
        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.messages[0].role, Role::User);
    }

    #[test]
    fn serde_round_trip() {
        let mut ctx = Context::default();
        ctx.push_user_text("test").unwrap();
        ctx.push_assistant(vec![ContentBlock::Text {
            text: "response".into(),
        }]).unwrap();
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
            ContentBlock::Text { text: "calling tool".into() },
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "/tmp/test"}),
            },
        ]).unwrap();
        ctx.push_tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: "file contents".into(),
            is_error: false,
        }]).unwrap();
        let json = serde_json::to_string(&ctx).expect("serialize");
        let restored: Context = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.messages.len(), 2);
        assert!(matches!(&restored.messages[0].content[1], ContentBlock::ToolUse { name, .. } if name == "read_file"));
        assert!(matches!(&restored.messages[1].content[0], ContentBlock::ToolResult { content, is_error, .. } if content == "file contents" && !is_error));
    }

    #[test]
    fn serde_round_trip_thinking_empty_signature() {
        let mut ctx = Context::default();
        ctx.push_assistant(vec![ContentBlock::Thinking {
            text: "reasoning".into(),
            signature: String::new(),
        }]).unwrap();
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
        let result = Context::load_from_file(std::path::Path::new("/nonexistent/context.json")).await;
        assert!(matches!(result, Err(crate::error::FlickError::Io(_))));
    }

    #[tokio::test]
    async fn load_from_file_invalid_json() {
        use std::io::Write;

        let mut f = tempfile::NamedTempFile::new().expect("create temp file");
        f.write_all(b"not json").expect("write");
        let result = Context::load_from_file(f.path()).await;
        assert!(matches!(result, Err(crate::error::FlickError::ContextParse(_))));
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
        assert!(matches!(result, Err(crate::error::FlickError::ContextParse(_))));
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
}
