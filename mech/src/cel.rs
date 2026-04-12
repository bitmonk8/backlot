//! CEL expression compilation, evaluation, and template interpolation.
//!
//! This module wraps [`cel_interpreter`] with mech-specific namespace
//! management and `{{ ... }}` template interpolation as described in
//! `docs/MECH_SPEC.md` §6.3 and §7.
//!
//! # Namespaces
//!
//! Per Deliverable 3 of the mech implementation plan (spec §13), five
//! namespaces are bound at evaluation time:
//!
//! * `input`     — function or block input
//! * `context`   — function-local declared variables
//! * `workflow`  — workflow-level declared variables
//! * `block`     — prior block outputs keyed by block name
//! * `meta`      — workflow/run metadata
//!
//! Note: the canonical spec §7 uses `blocks` (plural) and a separate
//! post-execution `output` namespace for the *current* block. D3's scope
//! collapses those into `block` (singular, prior outputs) plus `meta`, which
//! is what this module implements. Deliverable 8 (context & state
//! management) will reconcile the two names; this comment exists to keep the
//! discrepancy visible until then.
//!
//! # Compilation is once, evaluation is pure
//!
//! [`CelExpression`] is the compiled form of a bare expression, and
//! [`Template`] is the compiled form of a possibly-interpolated string. Both
//! are constructed once at workflow load time; evaluation is a pure function
//! of the supplied [`Namespaces`].

use std::fmt;

use std::collections::BTreeMap;

use cel_interpreter::{Context, ExecutionError, Program, Value, to_value};
use serde_json::Value as JsonValue;

use crate::error::{MechError, MechResult};

/// The five mech namespaces bound into a CEL evaluation context.
///
/// Each namespace holds an arbitrary JSON value (typically an object). Fields
/// default to `JsonValue::Null` when not supplied, which CEL will still allow
/// top-level access to but will error on nested field access — matching the
/// "missing field names the path" requirement.
#[derive(Debug, Clone, Default)]
pub struct Namespaces {
    /// `input` namespace — function or block input.
    pub input: JsonValue,
    /// `context` namespace — function-local declared variables.
    pub context: JsonValue,
    /// `workflow` namespace — workflow-level declared variables.
    pub workflow: JsonValue,
    /// `block` namespace — prior block outputs keyed by block name.
    pub block: JsonValue,
    /// `meta` namespace — workflow/run metadata.
    pub meta: JsonValue,
    /// Additional top-level CEL variables beyond the five standard
    /// namespaces. Used by call block output mappings to expose
    /// function results as `<fn_name>.output.*`.
    pub extras: BTreeMap<String, JsonValue>,
}

impl Namespaces {
    /// Construct an empty [`Namespaces`] with all five fields set to
    /// `JsonValue::Null`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Construct from five JSON values.
    pub fn new(
        input: JsonValue,
        context: JsonValue,
        workflow: JsonValue,
        block: JsonValue,
        meta: JsonValue,
    ) -> Self {
        Self {
            input,
            context,
            workflow,
            block,
            meta,
            extras: BTreeMap::new(),
        }
    }

    /// Construct with additional top-level CEL variables.
    pub fn with_extras(
        input: JsonValue,
        context: JsonValue,
        workflow: JsonValue,
        block: JsonValue,
        meta: JsonValue,
        extras: BTreeMap<String, JsonValue>,
    ) -> Self {
        Self {
            input,
            context,
            workflow,
            block,
            meta,
            extras,
        }
    }

    /// Build a fresh [`cel_interpreter::Context`] with the five namespaces
    /// bound as top-level variables.
    fn to_context(&self) -> MechResult<Context<'static>> {
        let mut ctx = Context::default();
        for (name, json) in [
            ("input", &self.input),
            ("context", &self.context),
            ("workflow", &self.workflow),
            ("block", &self.block),
            // Alias: spec §7 uses `blocks` (plural), runtime uses `block`.
            // Bind both so templates work with either form.
            ("blocks", &self.block),
            ("meta", &self.meta),
        ] {
            let value = to_value(json).map_err(|e| MechError::CelEvaluation {
                source_text: format!("<namespace {name}>"),
                message: format!("failed to convert JSON to CEL value: {e}"),
            })?;
            let value = normalize_uint_to_int(value);
            ctx.add_variable_from_value(name, value);
        }
        for (name, json) in &self.extras {
            let value = to_value(json).map_err(|e| MechError::CelEvaluation {
                source_text: format!("<extra {name}>"),
                message: format!("failed to convert JSON to CEL value: {e}"),
            })?;
            let value = normalize_uint_to_int(value);
            ctx.add_variable_from_value(name, value);
        }
        Ok(ctx)
    }
}

/// Recursively convert `Value::UInt` to `Value::Int` throughout a CEL value
/// tree. serde_json stores non-negative integers as `u64`, which
/// `cel_interpreter::to_value` maps to `Value::UInt`. CEL does not support
/// mixed `UInt`/`Int` arithmetic (e.g. `UInt(0) + Int(1)` errors), so we
/// normalize everything to `Int` for consistent behavior.
fn normalize_uint_to_int(value: Value) -> Value {
    match value {
        Value::UInt(n) => Value::Int(n as i64),
        Value::List(items) => {
            let new_items: Vec<Value> = items.iter().cloned().map(normalize_uint_to_int).collect();
            Value::List(std::sync::Arc::new(new_items))
        }
        Value::Map(map) => {
            let new_map: std::collections::HashMap<cel_interpreter::objects::Key, Value> = map
                .map
                .iter()
                .map(|(k, v)| (k.clone(), normalize_uint_to_int(v.clone())))
                .collect();
            Value::Map(cel_interpreter::objects::Map {
                map: std::sync::Arc::new(new_map),
            })
        }
        other => other,
    }
}

/// Convert a [`cel_interpreter::Value`] to a [`serde_json::Value`].
pub fn cel_value_to_json(value: &Value) -> MechResult<JsonValue> {
    match value {
        Value::Null => Ok(JsonValue::Null),
        Value::Bool(b) => Ok(JsonValue::Bool(*b)),
        Value::Int(n) => Ok(JsonValue::Number((*n).into())),
        Value::UInt(n) => Ok(JsonValue::Number((*n).into())),
        Value::Float(n) => {
            let num = serde_json::Number::from_f64(*n).ok_or_else(|| MechError::CelEvaluation {
                source_text: "<value conversion>".into(),
                message: format!("cannot represent float {n} as JSON number"),
            })?;
            Ok(JsonValue::Number(num))
        }
        Value::String(s) => Ok(JsonValue::String(s.to_string())),
        _ => {
            // Lists, maps, etc. — use the cel Value's .json() method.
            value.json().map_err(|e| MechError::CelEvaluation {
                source_text: "<value conversion>".into(),
                message: format!("cannot convert CEL value to JSON: {e}"),
            })
        }
    }
}

/// A compiled CEL expression.
///
/// Construct once at workflow load time via [`CelExpression::compile`] and
/// reuse across evaluations. Holds the original source text for error
/// reporting.
#[derive(Debug)]
pub struct CelExpression {
    source: String,
    program: Program,
}

impl CelExpression {
    /// Compile a CEL expression from source text.
    pub fn compile(source: impl Into<String>) -> MechResult<Self> {
        let source = source.into();
        let program = Program::compile(&source).map_err(|e| MechError::CelCompilation {
            source_text: source.clone(),
            message: e.to_string(),
        })?;
        Ok(Self { source, program })
    }

    /// The original source text of the expression.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Evaluate the expression against the provided namespaces.
    pub fn evaluate(&self, namespaces: &Namespaces) -> MechResult<Value> {
        let ctx = namespaces.to_context()?;
        self.program
            .execute(&ctx)
            .map_err(|e| MechError::CelEvaluation {
                source_text: self.source.clone(),
                message: format_execution_error(&e),
            })
    }

    /// Evaluate the expression as a boolean guard.
    ///
    /// Returns an error if the expression evaluates to a non-bool value.
    pub fn evaluate_guard(&self, namespaces: &Namespaces) -> MechResult<bool> {
        match self.evaluate(namespaces)? {
            Value::Bool(b) => Ok(b),
            other => Err(MechError::CelType {
                source_text: self.source.clone(),
                expected: "bool".into(),
                got: value_type_name(&other).into(),
            }),
        }
    }
}

// ---- Templates ------------------------------------------------------------

/// One segment of a compiled [`Template`].
#[derive(Debug)]
enum Segment {
    /// A literal string (already unescaped).
    Literal(String),
    /// A compiled `{{ ... }}` expression.
    Expr(CelExpression),
}

/// A compiled template string with zero or more `{{ ... }}` expression
/// segments.
///
/// Template syntax (spec §7):
///
/// * Expressions are delimited by `{{` and `}}`.
/// * Literal braces are produced by a CEL string expression:
///   `{{"{"}}` and `{{"}"}}` render as `{` and `}` respectively.
/// * Expression results are serialized using CEL -> JSON, with strings
///   rendered without surrounding quotes.
#[derive(Debug)]
pub struct Template {
    source: String,
    segments: Vec<Segment>,
}

impl Template {
    /// Compile a template string.
    ///
    /// Scans for `{{ ... }}` segments, compiles each as a CEL expression, and
    /// stores literal segments verbatim.
    pub fn compile(source: impl Into<String>) -> MechResult<Self> {
        let source = source.into();
        let segments = parse_template(&source)?;
        Ok(Self { source, segments })
    }

    /// The original source text of the template.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Render the template against the given namespaces, concatenating
    /// literals and expression results.
    pub fn render(&self, namespaces: &Namespaces) -> MechResult<String> {
        let mut out = String::with_capacity(self.source.len());
        for seg in &self.segments {
            match seg {
                Segment::Literal(s) => out.push_str(s),
                Segment::Expr(expr) => {
                    let value = expr.evaluate(namespaces)?;
                    append_rendered(&mut out, &value, expr.source())?;
                }
            }
        }
        Ok(out)
    }

    /// Evaluate the template as a JSON value.
    ///
    /// For templates that consist of a single `{{expr}}` expression with no
    /// surrounding literal text, this returns the CEL expression result
    /// converted to JSON (preserving its native type: number, boolean,
    /// object, array, etc.). For mixed or literal-only templates, this
    /// returns `JsonValue::String` of the rendered output.
    pub fn evaluate_as_json(&self, namespaces: &Namespaces) -> MechResult<JsonValue> {
        // Pure single-expression template: preserve the CEL type.
        if self.segments.len() == 1 {
            if let Segment::Expr(expr) = &self.segments[0] {
                let value = expr.evaluate(namespaces)?;
                return cel_value_to_json(&value);
            }
        }
        // Mixed or literal-only: render as string.
        let rendered = self.render(namespaces)?;
        Ok(JsonValue::String(rendered))
    }
}

fn parse_template(source: &str) -> MechResult<Vec<Segment>> {
    let bytes = source.as_bytes();
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Start of an expression; flush literal and scan for closing `}}`.
            if !literal.is_empty() {
                segments.push(Segment::Literal(std::mem::take(&mut literal)));
            }
            let start = i + 2;
            let mut j = start;
            let mut found = false;
            // Track CEL string literal state while scanning for `}}`. Raw
            // strings (`r"..."`) are not specially handled; backslashes are
            // ignored inside them by CEL, but for the purpose of finding the
            // matching quote our `\` escape logic is a superset that still
            // terminates correctly on the matching quote.
            let mut string_quote: Option<u8> = None;
            let mut brace_depth: i32 = 0;
            while j + 1 < bytes.len() {
                let b = bytes[j];
                if let Some(q) = string_quote {
                    if b == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    if b == q {
                        string_quote = None;
                    }
                    j += 1;
                    continue;
                }
                if b == b'"' || b == b'\'' {
                    string_quote = Some(b);
                    j += 1;
                    continue;
                }
                if b == b'{' {
                    brace_depth += 1;
                    j += 1;
                    continue;
                }
                if b == b'}' && bytes[j + 1] == b'}' && brace_depth == 0 {
                    found = true;
                    break;
                }
                if b == b'}' {
                    brace_depth -= 1;
                    j += 1;
                    continue;
                }
                j += 1;
            }
            if !found {
                return Err(MechError::TemplateParse {
                    source_text: source.to_string(),
                    message: format!("unterminated `{{{{` at byte offset {i}"),
                });
            }
            let expr_src = &source[start..j];
            let trimmed = expr_src.trim();
            if trimmed.is_empty() {
                return Err(MechError::TemplateParse {
                    source_text: source.to_string(),
                    message: format!("empty expression at byte offset {i}"),
                });
            }
            let expr = CelExpression::compile(trimmed)?;
            segments.push(Segment::Expr(expr));
            i = j + 2;
        } else {
            // Consume one UTF-8 scalar at a time.
            let ch_len = utf8_char_len(bytes[i]);
            literal.push_str(&source[i..i + ch_len]);
            i += ch_len;
        }
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

fn utf8_char_len(first: u8) -> usize {
    // Stray continuation bytes (0x80..0xC0) are treated as length 1 to make
    // forward progress on malformed input rather than panicking.
    if first < 0xC0 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

fn append_rendered(out: &mut String, value: &Value, source_text: &str) -> MechResult<()> {
    // Strings render without surrounding quotes; everything else via JSON.
    match value {
        Value::String(s) => out.push_str(s),
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::UInt(n) => out.push_str(&n.to_string()),
        Value::Float(n) => out.push_str(&n.to_string()),
        _ => {
            let json = value.json().map_err(|e| MechError::CelEvaluation {
                source_text: source_text.to_string(),
                message: format!("cannot render value as JSON: {e}"),
            })?;
            out.push_str(&json.to_string());
        }
    }
    Ok(())
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::List(_) => "list",
        Value::Map(_) => "map",
        Value::Function(_, _) => "function",
        Value::Int(_) => "int",
        Value::UInt(_) => "uint",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Bytes(_) => "bytes",
        Value::Bool(_) => "bool",
        Value::Duration(_) => "duration",
        Value::Timestamp(_) => "timestamp",
        Value::Null => "null",
    }
}

fn format_execution_error(e: &ExecutionError) -> String {
    // The default Display for ExecutionError names the missing key/variable,
    // which satisfies the "name the path" requirement. Include the variant so
    // callers can match on specifics in tests.
    match e {
        ExecutionError::NoSuchKey(k) => format!("no such key: {k}"),
        ExecutionError::UndeclaredReference(name) => {
            format!("undeclared reference to `{name}`")
        }
        other => other.to_string(),
    }
}

impl fmt::Display for CelExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ns() -> Namespaces {
        Namespaces::new(
            json!({ "name": "world", "n": 3 }),
            json!({ "count": 7 }),
            json!({ "budget": 100 }),
            // Block namespace wraps each block's value under `output` to match
            // the spec's `blocks.<name>.output.<field>` access pattern.
            json!({ "extract": { "output": { "ok": true, "items": [1, 2, 3] } } }),
            json!({ "run_id": "abc" }),
        )
    }

    #[test]
    fn compiles_arithmetic_field_access_and_methods() {
        let e = CelExpression::compile("1 + 2 * 3").unwrap();
        assert_eq!(e.evaluate(&Namespaces::empty()).unwrap(), Value::Int(7));

        let e = CelExpression::compile("input.n + context.count").unwrap();
        assert_eq!(e.evaluate(&ns()).unwrap(), Value::Int(10));

        let e = CelExpression::compile("size(block.extract.output.items)").unwrap();
        assert_eq!(e.evaluate(&ns()).unwrap(), Value::Int(3));
    }

    #[test]
    fn rejects_invalid_cel_at_compile_time_with_source() {
        let err = CelExpression::compile("1 + ").unwrap_err();
        match err {
            MechError::CelCompilation { source_text, .. } => assert_eq!(source_text, "1 + "),
            other => panic!("expected CelCompilation, got {other:?}"),
        }
    }

    #[test]
    fn each_namespace_independently_accessible() {
        for (src, want) in [
            ("input.name", Value::String("world".to_string().into())),
            ("context.count", Value::Int(7)),
            ("workflow.budget", Value::Int(100)),
            ("block.extract.output.ok", Value::Bool(true)),
            ("meta.run_id", Value::String("abc".to_string().into())),
        ] {
            let e = CelExpression::compile(src).unwrap();
            assert_eq!(e.evaluate(&ns()).unwrap(), want, "for {src}");
        }
    }

    #[test]
    fn template_simple_interpolation() {
        let t = Template::compile("hello {{input.name}}").unwrap();
        assert_eq!(t.render(&ns()).unwrap(), "hello world");
    }

    #[test]
    fn template_multiple_expressions() {
        let t =
            Template::compile("{{input.name}}: {{context.count}} / {{workflow.budget}}").unwrap();
        assert_eq!(t.render(&ns()).unwrap(), "world: 7 / 100");
    }

    #[test]
    fn template_escaped_braces_via_string_exprs() {
        // Per spec §7: literal braces are produced by a CEL string expression.
        let t = Template::compile(r#"{{"{"}}ok{{"}"}}"#).unwrap();
        assert_eq!(t.render(&Namespaces::empty()).unwrap(), "{ok}");
    }

    #[test]
    fn template_nested_field_access() {
        let t = Template::compile("items={{block.extract.output.items}}").unwrap();
        // List values render as compact JSON.
        assert_eq!(t.render(&ns()).unwrap(), "items=[1,2,3]");
    }

    #[test]
    fn template_with_no_expressions_is_literal() {
        let t = Template::compile("plain string").unwrap();
        assert_eq!(t.render(&ns()).unwrap(), "plain string");
    }

    #[test]
    fn template_unterminated_errors() {
        let err = Template::compile("hello {{input.name").unwrap_err();
        assert!(
            matches!(err, MechError::TemplateParse { .. }),
            "expected TemplateParse, got {err:?}"
        );
    }

    #[test]
    fn template_empty_expression_errors() {
        let err = Template::compile("bad {{  }} seg").unwrap_err();
        assert!(matches!(err, MechError::TemplateParse { .. }));
    }

    #[test]
    fn guard_returning_non_bool_errors() {
        let e = CelExpression::compile("1 + 2").unwrap();
        let err = e.evaluate_guard(&ns()).unwrap_err();
        match err {
            MechError::CelType {
                expected,
                got,
                source_text,
            } => {
                assert_eq!(expected, "bool");
                assert_eq!(got, "int");
                assert_eq!(source_text, "1 + 2");
            }
            other => panic!("expected CelType, got {other:?}"),
        }
    }

    #[test]
    fn guard_returning_bool_works() {
        let e = CelExpression::compile("context.count > 5").unwrap();
        assert!(e.evaluate_guard(&ns()).unwrap());
        let e = CelExpression::compile("context.count > 100").unwrap();
        assert!(!e.evaluate_guard(&ns()).unwrap());
    }

    #[test]
    fn missing_namespace_field_error_names_the_path() {
        let e = CelExpression::compile("input.nope").unwrap();
        let err = e.evaluate(&ns()).unwrap_err();
        match err {
            MechError::CelEvaluation {
                source_text,
                message,
            } => {
                assert_eq!(source_text, "input.nope");
                assert!(
                    message.contains("nope"),
                    "expected error message to name missing field, got: {message}"
                );
            }
            other => panic!("expected CelEvaluation, got {other:?}"),
        }
    }

    #[test]
    fn missing_top_level_namespace_field_also_names_path() {
        // `context.missing` on an empty `context` namespace.
        let e = CelExpression::compile("context.missing").unwrap();
        let err = e.evaluate(&Namespaces::empty()).unwrap_err();
        match err {
            MechError::CelEvaluation { message, .. } => {
                assert!(
                    message.contains("missing"),
                    "expected missing field name in error, got: {message}"
                );
            }
            other => panic!("expected CelEvaluation, got {other:?}"),
        }
    }

    #[test]
    fn type_coercion_matches_cel_spec() {
        // CEL does not implicitly coerce int <-> string; these must be explicit.
        let e = CelExpression::compile("string(input.n) + \"!\"").unwrap();
        assert_eq!(
            e.evaluate(&ns()).unwrap(),
            Value::String("3!".to_string().into())
        );

        // int(string) parse
        let e = CelExpression::compile("int(\"42\") + 1").unwrap();
        assert_eq!(e.evaluate(&ns()).unwrap(), Value::Int(43));

        // bool && bool
        let e = CelExpression::compile("true && false").unwrap();
        assert_eq!(e.evaluate(&ns()).unwrap(), Value::Bool(false));

        // Implicit int -> string concat fails per CEL spec.
        let e = CelExpression::compile("input.n + \"x\"").unwrap();
        assert!(e.evaluate(&ns()).is_err());
    }

    #[test]
    fn artifacts_evaluate_against_namespaces() {
        let n = ns();
        let e = CelExpression::compile("workflow.budget - context.count").unwrap();
        assert_eq!(e.evaluate(&n).unwrap(), Value::Int(93));
        let g = CelExpression::compile("workflow.budget > 0").unwrap();
        assert!(g.evaluate_guard(&n).unwrap());
        let t = Template::compile("budget={{workflow.budget}}").unwrap();
        assert_eq!(t.render(&n).unwrap(), "budget=100");
    }

    #[test]
    fn compilation_is_reusable_across_evaluations() {
        let e = CelExpression::compile("int(input.n) * 2").unwrap();
        let mut n = ns();
        assert_eq!(e.evaluate(&n).unwrap(), Value::Int(6));
        n.input = json!({ "n": 50 });
        assert_eq!(e.evaluate(&n).unwrap(), Value::Int(100));
    }

    #[test]
    fn template_with_double_brace_inside_string_literal() {
        let t = Template::compile("{{\"}}\"}}").unwrap();
        assert_eq!(t.render(&Namespaces::empty()).unwrap(), "}}");
    }

    #[test]
    fn template_with_cel_map_literal() {
        let t = Template::compile(r#"{{ size({"a": 1, "b": 2}) }}"#).unwrap();
        assert_eq!(t.render(&Namespaces::empty()).unwrap(), "2");
    }

    #[test]
    fn template_with_single_quoted_string_containing_braces() {
        let t = Template::compile("{{'{{'}}").unwrap();
        assert_eq!(t.render(&Namespaces::empty()).unwrap(), "{{");
    }
}
