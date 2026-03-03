use serde::{Deserialize, Serialize};
use std::io::{BufWriter, Write};

// -- Events ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ThinkingSignature {
        signature: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        success: bool,
        output: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default, skip_serializing_if = "is_zero")]
        cache_creation_input_tokens: u64,
        #[serde(default, skip_serializing_if = "is_zero")]
        cache_read_input_tokens: u64,
    },
    Done {
        usage: RunSummary,
    },
    Error {
        message: String,
        code: String,
        /// True for errors that should abort the agent loop (API errors, auth
        /// failures). False for non-fatal operational conditions like
        /// `max_tokens` truncation or content filter hits.
        fatal: bool,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_hash: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires &T
const fn is_zero(v: &u64) -> bool {
    *v == 0
}

// -- Event emitters --------------------------------------------------------

pub trait EventEmitter {
    fn emit(&mut self, event: &Event);
}

/// Emits one JSON object per line to the given writer.
pub struct JsonLinesEmitter<W: Write> {
    writer: BufWriter<W>,
    broken: bool,
}

impl<W: Write> JsonLinesEmitter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: BufWriter::new(writer),
            broken: false,
        }
    }
}

impl<W: Write> EventEmitter for JsonLinesEmitter<W> {
    fn emit(&mut self, event: &Event) {
        if self.broken {
            return;
        }
        // Serialization of Event should never fail; if it does we
        // silently drop the event rather than panicking on stdout I/O.
        if serde_json::to_writer(&mut self.writer, event).is_err() {
            self.broken = true;
            return;
        }
        if writeln!(self.writer).is_err() || self.writer.flush().is_err() {
            self.broken = true;
        }
    }
}

/// Emits plain text for `text` events, ignores structured events.
pub struct RawEmitter<W: Write> {
    writer: BufWriter<W>,
    broken: bool,
}

impl<W: Write> RawEmitter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: BufWriter::new(writer),
            broken: false,
        }
    }
}

impl<W: Write> EventEmitter for RawEmitter<W> {
    fn emit(&mut self, event: &Event) {
        if self.broken {
            return;
        }
        let result = match event {
            Event::Text { text } => {
                write!(self.writer, "{text}").and_then(|()| self.writer.flush())
            }
            Event::Done { .. } => {
                writeln!(self.writer).and_then(|()| self.writer.flush())
            }
            Event::Error { message, fatal, .. } => {
                let prefix = if *fatal { "Error" } else { "Warning" };
                writeln!(self.writer, "\n{prefix}: {message}").and_then(|()| self.writer.flush())
            }
            _ => return,
        };
        if result.is_err() {
            self.broken = true;
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn json_lines_emitter_writes_valid_json() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::Text {
                text: "hello".into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "text");
        assert_eq!(parsed["text"], "hello");
    }

    #[test]
    fn json_lines_emitter_newline_terminated() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::Text {
                text: "a".into(),
            });
            emitter.emit(&Event::Text {
                text: "b".into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        assert_eq!(output.trim().lines().count(), 2);
    }

    #[test]
    fn raw_emitter_outputs_text() {
        let mut buf = Vec::new();
        {
            let mut emitter = RawEmitter::new(&mut buf);
            emitter.emit(&Event::Text {
                text: "hello".into(),
            });
            emitter.emit(&Event::Text {
                text: " world".into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        assert_eq!(output, "hello world");
    }

    #[test]
    fn raw_emitter_ignores_structured_events() {
        let mut buf = Vec::new();
        {
            let mut emitter = RawEmitter::new(&mut buf);
            emitter.emit(&Event::Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            });
            emitter.emit(&Event::ToolCall {
                call_id: "c1".into(),
                tool_name: "read_file".into(),
                arguments: "{}".into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        assert!(output.is_empty());
    }

    #[test]
    fn json_lines_emitter_done_event() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::Done {
                usage: RunSummary {
                    input_tokens: 100,
                    output_tokens: 50,
                    cost_usd: 0.005,
                    iterations: 2,
                    context_hash: None,
                },
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "done");
        assert_eq!(parsed["usage"]["input_tokens"], 100);
        assert_eq!(parsed["usage"]["output_tokens"], 50);
        assert_eq!(parsed["usage"]["iterations"], 2);
    }

    #[test]
    fn raw_emitter_error_event() {
        let mut buf = Vec::new();
        {
            let mut emitter = RawEmitter::new(&mut buf);
            emitter.emit(&Event::Error {
                message: "something broke".into(),
                code: "test_error".into(),
                fatal: true,
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        assert!(output.contains("something broke"));
    }

    #[test]
    fn json_lines_emitter_write_failure_does_not_panic() {
        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("write failed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        let mut emitter = JsonLinesEmitter::new(FailWriter);
        // Should not panic
        emitter.emit(&Event::Text { text: "test".into() });
    }

    #[test]
    fn json_lines_emitter_tool_call() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::ToolCall {
                call_id: "tc_1".into(),
                tool_name: "read_file".into(),
                arguments: r#"{"path":"/tmp"}"#.into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "tool_call");
        assert_eq!(parsed["call_id"], "tc_1");
        assert_eq!(parsed["tool_name"], "read_file");
        assert_eq!(parsed["arguments"], r#"{"path":"/tmp"}"#);
    }

    #[test]
    fn json_lines_emitter_tool_result() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::ToolResult {
                call_id: "tc_1".into(),
                success: true,
                output: "file contents".into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "tool_result");
        assert_eq!(parsed["success"], true);
    }

    #[test]
    fn json_lines_emitter_thinking_and_signature() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::Thinking { text: "hmm".into() });
            emitter.emit(&Event::ThinkingSignature { signature: "sig_abc".into() });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        let p1: serde_json::Value = serde_json::from_str(lines[0]).expect("valid JSON");
        assert_eq!(p1["type"], "thinking");
        assert_eq!(p1["text"], "hmm");
        let p2: serde_json::Value = serde_json::from_str(lines[1]).expect("valid JSON");
        assert_eq!(p2["type"], "thinking_signature");
        assert_eq!(p2["signature"], "sig_abc");
    }

    #[test]
    fn json_lines_emitter_error_event() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::Error {
                message: "something went wrong".into(),
                code: "test_error".into(),
                fatal: true,
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "error");
        assert_eq!(parsed["message"], "something went wrong");
        assert_eq!(parsed["code"], "test_error");
    }

    #[test]
    fn raw_emitter_write_failure_does_not_panic() {
        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("write failed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("flush failed"))
            }
        }
        let mut emitter = RawEmitter::new(FailWriter);
        // None of these should panic
        emitter.emit(&Event::Text { text: "test".into() });
        emitter.emit(&Event::Done { usage: RunSummary::default() });
        emitter.emit(&Event::Error { message: "err".into(), code: "e".into(), fatal: true });
    }

    #[test]
    fn raw_emitter_thinking_ignored() {
        let mut buf = Vec::new();
        {
            let mut emitter = RawEmitter::new(&mut buf);
            emitter.emit(&Event::Thinking {
                text: "reasoning step".into(),
            });
            emitter.emit(&Event::ThinkingSignature {
                signature: "sig_abc".into(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        assert!(output.is_empty(), "thinking events should not produce output in raw mode");
    }

    #[test]
    fn json_lines_emitter_usage_standalone() {
        let mut buf = Vec::new();
        {
            let mut emitter = JsonLinesEmitter::new(&mut buf);
            emitter.emit(&Event::Usage {
                input_tokens: 42,
                output_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON");
        assert_eq!(parsed["type"], "usage");
        assert_eq!(parsed["input_tokens"], 42);
        assert_eq!(parsed["output_tokens"], 7);
    }

    #[test]
    fn raw_emitter_done_writes_newline() {
        let mut buf = Vec::new();
        {
            let mut emitter = RawEmitter::new(&mut buf);
            emitter.emit(&Event::Text {
                text: "end".into(),
            });
            emitter.emit(&Event::Done {
                usage: RunSummary::default(),
            });
        }
        let output = String::from_utf8(buf).expect("valid utf8");
        assert!(output.ends_with('\n'));
    }
}
