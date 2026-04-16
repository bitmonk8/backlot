//! Schema reference types: [`SchemaRef`].

use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use super::JsonValue;

/// A JSON Schema reference: inline, external/named ref, or the literal
/// `"infer"` (function output only).
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaRef {
    /// The string `"infer"` — requests automatic inference (function output).
    Infer,
    /// `$ref:#name` or `$ref:path`.
    Ref(String),
    /// Inline JSON Schema object.
    Inline(JsonValue),
}

impl Serialize for SchemaRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SchemaRef::Infer => serializer.serialize_str("infer"),
            SchemaRef::Ref(s) => serializer.serialize_str(s),
            SchemaRef::Inline(v) => v.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for SchemaRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => {
                if s == "infer" {
                    Ok(SchemaRef::Infer)
                } else {
                    Ok(SchemaRef::Ref(s))
                }
            }
            serde_json::Value::Object(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Bool(_)
            | serde_json::Value::Null => Ok(SchemaRef::Inline(value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_empty_string_as_ref() {
        let sr: SchemaRef = serde_yml::from_str(r#""""#).unwrap();
        assert_eq!(sr, SchemaRef::Ref("".to_string()));
    }

    #[test]
    fn deserialize_non_infer_string_as_ref() {
        let sr: SchemaRef = serde_yml::from_str(r#""hello""#).unwrap();
        assert_eq!(sr, SchemaRef::Ref("hello".to_string()));
    }

    #[test]
    fn deserialize_capital_infer_as_ref_not_infer() {
        let sr: SchemaRef = serde_yml::from_str(r#""Infer""#).unwrap();
        assert_eq!(sr, SchemaRef::Ref("Infer".to_string()));
    }

    #[test]
    fn deserialize_boolean_as_inline() {
        let sr: SchemaRef = serde_yml::from_str("true").unwrap();
        assert_eq!(sr, SchemaRef::Inline(serde_json::Value::Bool(true)));
    }
}
