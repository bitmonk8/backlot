//! Schema reference types: [`SchemaRef`] and [`InferLiteral`].

use serde::{Deserialize, Serialize};

use super::JsonValue;

/// A JSON Schema reference: inline, external/named ref, or the literal
/// `"infer"` (function output only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SchemaRef {
    /// The string `"infer"` — requests automatic inference (function output).
    Infer(InferLiteral),
    /// `$ref:#name` or `$ref:path`.
    Ref(String),
    /// Inline JSON Schema object.
    Inline(JsonValue),
}

/// Serialises as the literal string `"infer"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InferLiteral {
    /// The one and only inhabitant.
    #[serde(rename = "infer")]
    Infer,
}
