//! Conversation management and history scoping (Deliverable 13).
//!
//! Per `docs/MECH_SPEC.md` §4.6:
//!
//! * Each function invocation creates a fresh, empty conversation.
//! * Prompt blocks on control-flow paths accumulate history (user + assistant
//!   + tool call/result messages).
//! * Call blocks are conversation-transparent — callees start fresh.
//! * Dataflow blocks are single-turn (no shared history).
//! * Self-loops and backward edges accumulate history intentionally.
//! * Compaction hooks exist as an extension point (actual strategy is a
//!   no-op placeholder in this deliverable).

use serde::{Deserialize, Serialize};

use crate::schema::{CompactionConfig, FunctionDef, MechDocument};

/// Role of a message in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// System prompt — always the first message if present.
    System,
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
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }

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
    let cfg = function.compaction.as_ref().or_else(|| {
        workflow
            .workflow
            .as_ref()
            .and_then(|w| w.compaction.as_ref())
    });
    cfg.map(ResolvedCompaction::from)
}

/// Per-function conversation history.
///
/// Created fresh at each function invocation. Accumulates messages along
/// control-flow paths. Call blocks and dataflow blocks do not contribute
/// to or consume conversation history.
///
/// The optional system prompt lives in its own slot rather than as the first
/// element of `messages`. The function-entry render is the single source of
/// truth: it is supplied to the agent through `AgentRequest.system` rather
/// than the message history (which would otherwise duplicate the system
/// prompt for executors that prepend system to history themselves).
/// [`Self::len`] / [`Self::is_empty`] count message history only — use
/// [`Self::has_system`] separately when the system slot matters.
#[derive(Debug, Clone)]
pub struct Conversation {
    /// Pre-rendered system prompt, or `None` when no system is configured.
    /// Stored separately from `messages` to avoid double-rendering.
    system: Option<String>,
    messages: Vec<Message>,
    compaction: Option<ResolvedCompaction>,
    /// How many times compaction was triggered (for testing).
    compaction_count: usize,
}

impl Conversation {
    /// Create an empty conversation with no compaction and no system prompt.
    pub fn new() -> Self {
        Self {
            system: None,
            messages: Vec::new(),
            compaction: None,
            compaction_count: 0,
        }
    }

    /// Create a conversation pre-loaded with a rendered system prompt.
    ///
    /// The system prompt is stored in a dedicated slot — it does NOT appear
    /// in the message list. Prompt blocks read it via [`system()`].
    pub fn with_system(system: impl Into<String>) -> Self {
        Self {
            system: Some(system.into()),
            messages: Vec::new(),
            compaction: None,
            compaction_count: 0,
        }
    }

    /// The pre-rendered system prompt, if any.
    pub fn system(&self) -> Option<&str> {
        self.system.as_deref()
    }

    /// Whether this conversation carries a system prompt. Independent of
    /// the message history — use [`Self::is_empty`] / [`Self::len`] for
    /// history-only queries.
    pub fn has_system(&self) -> bool {
        self.system.is_some()
    }

    /// Set the compaction configuration.
    pub fn with_compaction(mut self, compaction: Option<ResolvedCompaction>) -> Self {
        self.compaction = compaction;
        self
    }

    /// Append a single message.
    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// Append multiple messages (e.g. tool call/result pairs from agent loop).
    pub fn push_many(&mut self, msgs: Vec<Message>) {
        self.messages.extend(msgs);
    }

    /// Read access to the message history (user / assistant / tool
    /// messages). Does NOT include the system prompt — the system slot is
    /// transported separately through [`Self::system`] /
    /// `AgentRequest.system`.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Number of messages in the history. Does NOT count the system slot —
    /// query [`Self::has_system`] separately if that matters.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the message history is empty. Does NOT consider the system
    /// slot — a conversation that only has a system prompt is still
    /// `is_empty() == true` from a history-count perspective. Use
    /// [`Self::has_system`] for the system query.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// How many times compaction was triggered.
    pub fn compaction_count(&self) -> usize {
        self.compaction_count
    }

    /// Check whether compaction should fire and record the trigger.
    ///
    /// This is a placeholder: D13 establishes the extension point but does
    /// not implement actual summarization. The method estimates token usage
    /// from message count (rough heuristic: 100 tokens per message) and
    /// triggers when the estimate exceeds the configured threshold. When
    /// triggered, it increments the compaction counter but does not modify
    /// the message list.
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

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn empty_conversation() {
        let conv = Conversation::new();
        assert!(conv.is_empty());
        assert_eq!(conv.len(), 0);
        assert_eq!(conv.messages(), &[]);
    }

    #[test]
    fn conversation_with_system() {
        // The system prompt lives in its own slot, not as the first element
        // of `messages()`. `len()` / `is_empty()` report history-only;
        // `has_system()` is the dedicated system query.
        let conv = Conversation::with_system("You are helpful.");
        assert_eq!(conv.system(), Some("You are helpful."));
        assert!(conv.has_system());
        assert!(
            conv.messages().is_empty(),
            "system must not appear in messages()"
        );
        assert_eq!(conv.len(), 0, "len() reports history only");
        assert!(conv.is_empty(), "is_empty() reports history only");
    }

    #[test]
    fn len_and_is_empty_reflect_history_only() {
        // No system, no messages.
        let mut conv = Conversation::new();
        assert_eq!(conv.len(), 0);
        assert!(conv.is_empty());
        assert!(!conv.has_system());

        // Push history without a system.
        conv.push(Message::user("hi"));
        assert_eq!(conv.len(), 1);
        assert!(!conv.is_empty());
        assert!(!conv.has_system());

        // System-only — len/is_empty still report the history.
        let conv = Conversation::with_system("sys");
        assert_eq!(conv.len(), 0, "history-only count ignores system slot");
        assert!(conv.is_empty(), "history-only is_empty ignores system slot");
        assert!(conv.has_system());

        // System + history.
        let mut conv = Conversation::with_system("sys");
        conv.push(Message::user("u"));
        conv.push(Message::assistant("a"));
        assert_eq!(conv.len(), 2, "len = 2 history messages");
        assert!(!conv.is_empty());
        assert!(conv.has_system());
        assert_eq!(conv.messages().len(), 2, "messages() returns history only");
    }

    #[test]
    fn push_and_push_many() {
        let mut conv = Conversation::new();
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
        let mut conv = Conversation::new();
        for i in 0..50 {
            conv.push(Message::user(format!("msg {i}")));
        }
        conv.check_compaction();
        assert_eq!(conv.compaction_count(), 0);
    }

    #[test]
    fn compaction_triggered_at_threshold() {
        let mut conv = Conversation::new().with_compaction(Some(ResolvedCompaction {
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
        let mut conv = Conversation::new().with_compaction(Some(ResolvedCompaction {
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
                compaction: Some(CompactionConfig {
                    keep_recent_tokens: 1000,
                    reserve_tokens: 2000,
                    func: None,
                }),
                ..Default::default()
            }),
            functions: BTreeMap::new(),
        };
        let func = crate::schema::FunctionDef {
            input: serde_json::json!({ "type": "object" }),
            output: None,
            system: None,
            agent: None,
            terminals: Vec::new(),
            context: BTreeMap::new(),
            compaction: Some(CompactionConfig {
                keep_recent_tokens: 500,
                reserve_tokens: 800,
                func: Some("custom".into()),
            }),
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
                compaction: Some(CompactionConfig {
                    keep_recent_tokens: 1000,
                    reserve_tokens: 2000,
                    func: None,
                }),
                ..Default::default()
            }),
            functions: BTreeMap::new(),
        };
        let func = crate::schema::FunctionDef {
            input: serde_json::json!({ "type": "object" }),
            output: None,
            system: None,
            agent: None,
            terminals: Vec::new(),
            context: BTreeMap::new(),
            compaction: None,
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
            system: None,
            agent: None,
            terminals: Vec::new(),
            context: BTreeMap::new(),
            compaction: None,
            blocks: BTreeMap::new(),
        };

        assert!(resolve_compaction(&wf, &func).is_none());
    }
}
