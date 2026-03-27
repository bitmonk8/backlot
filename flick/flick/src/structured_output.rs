use crate::context::ContentBlock;
use crate::error::FlickError;

/// Strip opening/closing markdown code fences if the text is wrapped in them.
fn strip_code_fences(text: &str) -> String {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") || !trimmed.ends_with("```") {
        return text.to_string();
    }
    let Some(first_newline) = trimmed.find('\n') else {
        return text.to_string();
    };
    let body = &trimmed[first_newline + 1..];
    body.rfind("```")
        .map_or_else(|| text.to_string(), |pos| body[..pos].trim().to_string())
}

/// Strip markdown code fences from all text blocks in place.
///
/// Models sometimes wrap structured output in `` ```json ... ``` `` fences.
pub fn strip_fences_from_blocks(blocks: &mut [ContentBlock]) {
    for block in blocks.iter_mut() {
        if let ContentBlock::Text { text } = block {
            *text = strip_code_fences(text);
        }
    }
}

/// Parse the first text block as JSON and check that all `required` fields
/// declared in the schema are present (recursively for nested objects and
/// array items).
///
/// Errors if no text block is found (structured output requires one).
pub fn check_required_fields(
    blocks: &[ContentBlock],
    schema: &serde_json::Value,
) -> Result<(), FlickError> {
    let text = blocks
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            FlickError::SchemaValidation(
                "structured output expected a text block but none was returned".into(),
            )
        })?;

    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(|e| FlickError::ResponseNotJson(e.to_string()))?;
    validate_required_fields(&parsed, schema, "")?;

    Ok(())
}

/// Join a parent path with a child segment (e.g. "" + "name" -> "name", "a" + "b" -> "a.b").
fn join_field_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}.{child}")
    }
}

/// Recursively validate that all `required` fields in the schema are present in the JSON value.
/// Descends into nested object properties and array items.
fn validate_required_fields(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<(), FlickError> {
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        let obj = value.as_object().ok_or_else(|| {
            FlickError::SchemaValidation(format!(
                "expected object at '{path}', got {}",
                value_type_name(value)
            ))
        })?;
        for field in required {
            if let Some(field_name) = field.as_str() {
                if !obj.contains_key(field_name) {
                    return Err(FlickError::SchemaValidation(format!(
                        "missing required field '{}'",
                        join_field_path(path, field_name)
                    )));
                }
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        if let Some(obj) = value.as_object() {
            for (prop_name, prop_schema) in properties {
                if let Some(prop_value) = obj.get(prop_name) {
                    validate_required_fields(
                        prop_value,
                        prop_schema,
                        &join_field_path(path, prop_name),
                    )?;
                }
            }
        }
    }

    if let Some(items_schema) = schema.get("items") {
        if let Some(arr) = value.as_array() {
            for (i, element) in arr.iter().enumerate() {
                let child_path = format!("{path}[{i}]");
                validate_required_fields(element, items_schema, &child_path)?;
            }
        }
    }

    Ok(())
}

const fn value_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn strip_fences_json_tag() {
        let input = "```json\n{\"answer\": \"hello\"}\n```";
        let mut blocks = vec![ContentBlock::Text {
            text: input.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } }
        });
        {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap();
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "{\"answer\": \"hello\"}"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn strip_fences_no_tag() {
        let input = "```\n{\"answer\": \"hello\"}\n```";
        let mut blocks = vec![ContentBlock::Text {
            text: input.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } }
        });
        {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap();
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "{\"answer\": \"hello\"}"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn no_fences_unchanged() {
        let input = "{\"answer\": \"hello\"}";
        let mut blocks = vec![ContentBlock::Text {
            text: input.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } }
        });
        {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap();
        match &blocks[0] {
            ContentBlock::Text { text } => assert_eq!(text, "{\"answer\": \"hello\"}"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn validates_required_fields_present() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"{"name": "Alice", "age": 30}"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        });
        {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap();
    }

    #[test]
    fn validates_required_fields_missing() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"{"name": "Alice"}"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(ref msg) if msg.contains("age")));
        assert_eq!(err.code(), "schema_validation");
    }

    #[test]
    fn validates_nested_required() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"{"address": {"city": "NYC"}}"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" },
                        "zip": { "type": "string" }
                    },
                    "required": ["city", "zip"]
                }
            },
            "required": ["address"]
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(msg) if msg.contains("address.zip")));
    }

    #[test]
    fn validates_array_items_required() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"[{"id": "a"}, {"name": "b"}]"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            }
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(msg) if msg.contains("[1].id")));
    }

    #[test]
    fn validates_array_items_all_valid() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"[{"id": "a"}, {"id": "b"}]"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }
                },
                "required": ["id"]
            }
        });
        {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap();
    }

    #[test]
    fn validates_nested_array_in_object() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"{"items": [{"id": 1}, {}]}"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id"],
                        "properties": { "id": { "type": "integer" } }
                    }
                }
            },
            "required": ["items"]
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(msg) if msg.contains("items[1].id")));
    }

    #[test]
    fn invalid_json_returns_response_not_json() {
        let mut blocks = vec![ContentBlock::Text {
            text: "not json at all".to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } }
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(matches!(err, FlickError::ResponseNotJson(_)));
        assert_eq!(err.code(), "response_not_json");
    }

    #[test]
    fn no_text_blocks_errors() {
        let mut blocks = vec![ContentBlock::Thinking {
            text: "hmm".into(),
            signature: String::new(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "required": ["answer"],
            "properties": { "answer": { "type": "string" } }
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(matches!(err, FlickError::SchemaValidation(msg) if msg.contains("text block")));
    }

    #[test]
    fn expected_object_got_scalar() {
        let mut blocks = vec![ContentBlock::Text {
            text: r#"{"address": "string_value"}"#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "required": ["city"],
                    "properties": { "city": { "type": "string" } }
                }
            },
            "required": ["address"]
        });
        let err = {
            strip_fences_from_blocks(&mut blocks);
            check_required_fields(&blocks, &schema)
        }
        .unwrap_err();
        assert!(
            matches!(err, FlickError::SchemaValidation(msg) if msg.contains("expected object at 'address'") && msg.contains("string"))
        );
    }

    // --- Group B tests: edge cases for strip_code_fences (#7) ---

    #[test]
    fn strip_fences_opening_only_unchanged() {
        let result = strip_code_fences("```json\n{\"a\": 1}");
        assert_eq!(result, "```json\n{\"a\": 1}");
    }

    #[test]
    fn strip_fences_single_line_backticks_unchanged() {
        let result = strip_code_fences("```");
        assert_eq!(result, "```");
    }

    #[test]
    fn strip_fences_leading_trailing_whitespace() {
        let result = strip_code_fences("  ```json\n{\"a\": 1}\n```  ");
        assert_eq!(result, "{\"a\": 1}");
    }

    #[test]
    fn strip_fences_nested_backticks_in_body() {
        let result = strip_code_fences("```\nsome ``` text\nactual content\n```");
        assert_eq!(result, "some ``` text\nactual content");
    }

    // --- #9: only the first Text block is validated ---

    #[test]
    fn check_required_fields_uses_first_text_block_only() {
        let blocks = vec![
            ContentBlock::Text {
                text: r#"{"answer": "ok"}"#.to_string(),
            },
            ContentBlock::Text {
                text: "not json at all".to_string(),
            },
        ];
        let schema = serde_json::json!({
            "type": "object",
            "required": ["answer"],
            "properties": { "answer": { "type": "string" } }
        });
        // Passes because only the first text block is checked.
        check_required_fields(&blocks, &schema).unwrap();
    }

    // --- #11: schema expects object with properties, value is scalar ---

    #[test]
    fn properties_without_required_on_non_object_passes() {
        let blocks = vec![ContentBlock::Text {
            text: r#""just a string""#.to_string(),
        }];
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        // No `required` array, so no type enforcement -- passes silently.
        check_required_fields(&blocks, &schema).unwrap();
    }
}
