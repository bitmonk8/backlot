//! Conversation management and history scoping.
//!
//! Per `docs/MECH_SPEC.md` §4.6:
//!
//! * Each function invocation creates a fresh, empty conversation.
//! * Prompt blocks on control-flow paths accumulate history (user + assistant
//!   + tool call/result messages).
//! * Call blocks are conversation-transparent — callees start fresh.
//! * Dataflow blocks are single-turn (no shared history).
//! * Self-loops and backward edges accumulate history intentionally.
//! * Compaction hooks exist as an extension point; the current strategy is a
//!   no-op placeholder.

use serde::{Deserialize, Serialize};

use crate::schema::{CompactionConfig, FunctionDef, MechDocument};

/// Role tag for messages stored in a [`Conversation`] and forwarded to
/// the agent via [`crate::exec::agent::AgentRequest::history`].
///
/// There is no `System` variant. The rendered system prompt is conveyed
/// out-of-band via [`crate::exec::agent::AgentRequest::system`] — never as
/// a message in conversation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// User message (rendered prompt template).
    User,
    /// Assistant response (LLM output).
    Assistant,
    /// Tool invocation from the agent's internal loop.
    ToolCall,
    /// Tool result from the agent's internal loop.
    ToolResult,
}

/// A single message in a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }

    pub fn tool_call(content: impl Into<String>) -> Self {
        Self {
            role: Role::ToolCall,
            content: content.into(),
        }
    }

    pub fn tool_result(content: impl Into<String>) -> Self {
        Self {
            role: Role::ToolResult,
            content: content.into(),
        }
    }
}

/// Resolved compaction configuration (from function-level or workflow default).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCompaction {
    /// Tokens of recent history to preserve verbatim.
    pub keep_recent_tokens: u32,
    /// Trigger threshold: fire when used > context_window - reserve.
    pub reserve_tokens: u32,
    /// Optional custom compaction function name.
    pub custom_fn: Option<String>,
}

impl From<&CompactionConfig> for ResolvedCompaction {
    fn from(cfg: &CompactionConfig) -> Self {
        Self {
            keep_recent_tokens: cfg.keep_recent_tokens,
            reserve_tokens: cfg.reserve_tokens,
            custom_fn: cfg.func.clone(),
        }
    }
}

/// Resolve compaction configuration: function-level overrides workflow-level.
/// Returns `None` if neither level declares compaction.
pub fn resolve_compaction(
    workflow: &MechDocument,
    function: &FunctionDef,
) -> Option<ResolvedCompaction> {
    let cfg = function
        .overrides
        .resolved_compaction(workflow.workflow.as_ref().map(|w| &w.defaults));
    cfg.map(ResolvedCompaction::from)
}

/// Per-function message history with optional compaction.
///
/// Created fresh at each function invocation and accumulated along
/// control-flow paths within an imperative function. Dataflow prompt
/// blocks run with their own fresh `Conversation` (§4.6 rule 3: data
/// edges do not carry history). Call blocks are conversation-transparent:
/// they own no `Conversation` and instead delegate to a sub-function
/// which gets its own fresh `Conversation` via `FunctionRunner`
/// (§4.6 rule 4).
///
/// `Default` is equivalent to `Conversation::new(None)`.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    messages: Vec<Message>,
    compaction: Option<ResolvedCompaction>,
    /// How many times compaction was triggered (for testing).
    compaction_count: usize,
}

impl Conversation {
    /// Create an empty conversation with the given compaction configuration
    /// (`None` to disable compaction).
    pub fn new(compaction: Option<ResolvedCompaction>) -> Self {
        Self {
            messages: Vec::new(),
            compaction,
            compaction_count: 0,
        }
    }

    /// Append a single message.
    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// Append multiple messages (e.g. tool call/result pairs from agent loop).
    pub fn push_many(&mut self, msgs: Vec<Message>) {
        self.messages.extend(msgs);
    }

    /// Read access to the message history.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Number of messages in the history.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the message history is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// How many times compaction was triggered.
    pub fn compaction_count(&self) -> usize {
        self.compaction_count
    }

    /// Check whether compaction should fire and record the trigger.
    ///
    /// Estimates token usage from message count (rough heuristic: 100 tokens
    /// per message) and triggers when the estimate exceeds the configured
    /// threshold. When triggered, it increments the compaction counter but
    /// does not modify the message list. Actual summarization is not yet
    /// implemented.
    pub fn check_compaction(&mut self) {
        let Some(cfg) = &self.compaction else {
            return;
        };
        // Rough heuristic: ~100 tokens per message. Real implementation will
        // use a proper tokenizer.
        let estimated_tokens = self.messages.len() as u32 * 100;
        let threshold = cfg.keep_recent_tokens + cfg.reserve_tokens;
        if estimated_tokens > threshold {
            self.compaction_count += 1;
            // Placeholder: actual compaction (summarize older messages) goes here.
        }
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn empty_conversation() {
        let conv = Conversation::new(None);
        assert!(conv.is_empty());
        assert_eq!(conv.len(), 0);
        assert_eq!(conv.messages(), &[]);
    }

    #[test]
    fn len_and_is_empty_track_history() {
        let mut conv = Conversation::new(None);
        assert_eq!(conv.len(), 0);
        assert!(conv.is_empty());

        conv.push(Message::user("hi"));
        assert_eq!(conv.len(), 1);
        assert!(!conv.is_empty());

        conv.push(Message::assistant("a"));
        assert_eq!(conv.len(), 2);
        assert_eq!(conv.messages().len(), 2);
    }

    #[test]
    fn push_and_push_many() {
        let mut conv = Conversation::new(None);
        conv.push(Message::user("hello"));
        conv.push(Message::assistant("hi"));
        conv.push_many(vec![
            Message::tool_call("search(query)"),
            Message::tool_result("result data"),
        ]);
        assert_eq!(conv.len(), 4);
        assert_eq!(conv.messages()[0].role, Role::User);
        assert_eq!(conv.messages()[1].role, Role::Assistant);
        assert_eq!(conv.messages()[2].role, Role::ToolCall);
        assert_eq!(conv.messages()[3].role, Role::ToolResult);
    }

    #[test]
    fn compaction_not_triggered_when_disabled() {
        let mut conv = Conversation::new(None);
        for i in 0..50 {
            conv.push(Message::user(format!("msg {i}")));
        }
        conv.check_compaction();
        assert_eq!(conv.compaction_count(), 0);
    }

    #[test]
    fn compaction_triggered_at_threshold() {
        let mut conv = Conversation::new(Some(ResolvedCompaction {
            keep_recent_tokens: 100,
            reserve_tokens: 100,
            custom_fn: None,
        }));
        // Threshold = 200 tokens. At 100 tokens/msg, 3 messages = 300 > 200.
        conv.push(Message::user("a"));
        conv.push(Message::assistant("b"));
        conv.push(Message::user("c"));
        conv.check_compaction();
        assert_eq!(conv.compaction_count(), 1);
        // Messages are NOT modified (placeholder).
        assert_eq!(conv.len(), 3);
    }

    #[test]
    fn compaction_not_triggered_below_threshold() {
        let mut conv = Conversation::new(Some(ResolvedCompaction {
            keep_recent_tokens: 5000,
            reserve_tokens: 5000,
            custom_fn: None,
        }));
        // Threshold = 10000 tokens. 2 messages = 200 < 10000.
        conv.push(Message::user("a"));
        conv.push(Message::assistant("b"));
        conv.check_compaction();
        assert_eq!(conv.compaction_count(), 0);
    }

    #[test]
    fn resolve_compaction_function_overrides_workflow() {
        let wf = crate::schema::MechDocument {
            workflow: Some(crate::schema::WorkflowSection {
                defaults: crate::schema::ExecutionConfig {
                    compaction: Some(CompactionConfig {
                        keep_recent_tokens: 1000,
                        reserve_tokens: 2000,
                        func: None,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            functions: BTreeMap::new(),
        };
        let func = crate::schema::FunctionDef {
            input: serde_json::json!({ "type": "object" }),
            output: None,
            overrides: crate::schema::ExecutionConfig {
                compaction: Some(CompactionConfig {
                    keep_recent_tokens: 500,
                    reserve_tokens: 800,
                    func: Some("custom".into()),
                }),
                ..Default::default()
            },
            terminals: Vec::new(),
            blocks: BTreeMap::new(),
        };

        let resolved = resolve_compaction(&wf, &func).unwrap();
        assert_eq!(resolved.keep_recent_tokens, 500);
        assert_eq!(resolved.reserve_tokens, 800);
        assert_eq!(resolved.custom_fn.as_deref(), Some("custom"));
    }

    #[test]
    fn resolve_compaction_falls_back_to_workflow() {
        let wf = crate::schema::MechDocument {
            workflow: Some(crate::schema::WorkflowSection {
                defaults: crate::schema::ExecutionConfig {
                    compaction: Some(CompactionConfig {
                        keep_recent_tokens: 1000,
                        reserve_tokens: 2000,
                        func: None,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            functions: BTreeMap::new(),
        };
        let func = crate::schema::FunctionDef {
            input: serde_json::json!({ "type": "object" }),
            output: None,
            overrides: crate::schema::ExecutionConfig::default(),
            terminals: Vec::new(),
            blocks: BTreeMap::new(),
        };

        let resolved = resolve_compaction(&wf, &func).unwrap();
        assert_eq!(resolved.keep_recent_tokens, 1000);
        assert_eq!(resolved.reserve_tokens, 2000);
    }

    #[test]
    fn resolve_compaction_none_when_unconfigured() {
        let wf = crate::schema::MechDocument {
            workflow: None,
            functions: BTreeMap::new(),
        };
        let func = crate::schema::FunctionDef {
            input: serde_json::json!({ "type": "object" }),
            output: None,
            overrides: crate::schema::ExecutionConfig::default(),
            terminals: Vec::new(),
            blocks: BTreeMap::new(),
        };

        assert!(resolve_compaction(&wf, &func).is_none());
    }
}
