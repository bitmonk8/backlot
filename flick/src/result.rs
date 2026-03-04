use serde::Serialize;

use crate::context::ContentBlock;

#[derive(Debug, Clone, Serialize)]
pub struct FlickResult {
    pub status: ResultStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResultError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Complete,
    ToolCallsPending,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_creation_input_tokens: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub cache_read_input_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultError {
    pub message: String,
    pub code: String,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires &T
const fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn serialize_complete_result() {
        let result = FlickResult {
            status: ResultStatus::Complete,
            content: vec![ContentBlock::Text {
                text: "Done.".into(),
            }],
            usage: Some(UsageSummary {
                input_tokens: 2400,
                output_tokens: 50,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                cost_usd: 0.0032,
            }),
            context_hash: Some("abc123".into()),
            error: None,
        };
        let json: serde_json::Value =
            serde_json::to_value(&result).expect("serialize");
        assert_eq!(json["status"], "complete");
        assert_eq!(json["content"][0]["text"], "Done.");
        assert_eq!(json["usage"]["input_tokens"], 2400);
        assert_eq!(json["context_hash"], "abc123");
        // error omitted when None
        assert!(json.get("error").is_none());
    }

    #[test]
    fn serialize_tool_calls_pending_result() {
        let result = FlickResult {
            status: ResultStatus::ToolCallsPending,
            content: vec![
                ContentBlock::Text {
                    text: "I'll read that file.".into(),
                },
                ContentBlock::ToolUse {
                    id: "tc_1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "src/main.rs"}),
                },
            ],
            usage: Some(UsageSummary {
                input_tokens: 1200,
                output_tokens: 340,
                cache_creation_input_tokens: 800,
                cache_read_input_tokens: 400,
                cost_usd: 0.0087,
            }),
            context_hash: Some("00a1b2c3".into()),
            error: None,
        };
        let json: serde_json::Value =
            serde_json::to_value(&result).expect("serialize");
        assert_eq!(json["status"], "tool_calls_pending");
        assert_eq!(json["content"].as_array().expect("content array").len(), 2);
        assert_eq!(json["usage"]["cache_creation_input_tokens"], 800);
        assert_eq!(json["usage"]["cache_read_input_tokens"], 400);
    }

    #[test]
    fn serialize_error_result() {
        let result = FlickResult {
            status: ResultStatus::Error,
            content: vec![],
            usage: None,
            context_hash: None,
            error: Some(ResultError {
                message: "Rate limit exceeded".into(),
                code: "rate_limit".into(),
            }),
        };
        let json: serde_json::Value =
            serde_json::to_value(&result).expect("serialize");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["message"], "Rate limit exceeded");
        assert_eq!(json["error"]["code"], "rate_limit");
        // content, usage, context_hash omitted when empty/None
        assert!(json.get("content").is_none());
        assert!(json.get("usage").is_none());
        assert!(json.get("context_hash").is_none());
    }

    #[test]
    fn usage_zero_cache_fields_omitted() {
        let usage = UsageSummary {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.001,
        };
        let json: serde_json::Value =
            serde_json::to_value(&usage).expect("serialize");
        assert!(json.get("cache_creation_input_tokens").is_none());
        assert!(json.get("cache_read_input_tokens").is_none());
        assert_eq!(json["input_tokens"], 100);
        assert_eq!(json["output_tokens"], 50);
    }

    #[test]
    fn usage_nonzero_cache_fields_present() {
        let usage = UsageSummary {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 200,
            cache_read_input_tokens: 300,
            cost_usd: 0.005,
        };
        let json: serde_json::Value =
            serde_json::to_value(&usage).expect("serialize");
        assert_eq!(json["cache_creation_input_tokens"], 200);
        assert_eq!(json["cache_read_input_tokens"], 300);
    }
}
