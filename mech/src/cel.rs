//! CEL expression compilation, evaluation, and template interpolation.
//!
//! This module wraps [`cel_interpreter`] with mech-specific namespace
//! management and `{{ ... }}` template interpolation as described in
//! `docs/MECH_SPEC.md` §6.3 and §7.
//!
//! # Namespaces
//!
//! Per spec §13, five namespaces are bound at evaluation time:
//!
//! * `input`     — function or block input
//! * `context`   — function-local declared variables
//! * `workflow`  — workflow-level declared variables
//! * `blocks`    — prior block outputs keyed by block name
//! * `meta`      — workflow/run metadata
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
    /// `blocks` namespace — prior block outputs keyed by block name.
    pub blocks: JsonValue,
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
        blocks: JsonValue,
        meta: JsonValue,
    ) -> Self {
        Self {
            input,
            context,
            workflow,
            blocks,
            meta,
            extras: BTreeMap::new(),
        }
    }

    /// Construct with additional top-level CEL variables.
    pub fn with_extras(
        input: JsonValue,
        context: JsonValue,
        workflow: JsonValue,
        blocks: JsonValue,
        meta: JsonValue,
        extras: BTreeMap<String, JsonValue>,
    ) -> Self {
        Self {
            input,
            context,
            workflow,
            blocks,
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
            ("block", &self.blocks),
            // Alias: spec §7 uses `blocks` (plural). Bind both so
            // templates work with either form.
            ("blocks", &self.blocks),
            ("meta", &self.meta),
        ] {
            let value = to_value(json).map_err(|e| MechError::CelNamespaceBind {
                namespace: name.to_string(),
                message: format!("failed to convert JSON to CEL value: {e}"),
            })?;
            let value = normalize_uint_to_int(value);
            ctx.add_variable_from_value(name, value);
        }
        for (name, json) in &self.extras {
            let value = to_value(json).map_err(|e| MechError::CelNamespaceBind {
                namespace: name.to_string(),
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
        Value::UInt(n) => i64::try_from(n).map_or(Value::UInt(n), Value::Int),
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

/// One raw segment of a template after grammar-only scanning.
///
/// This intermediate form is produced by [`scan_template_segments`] and
/// consumed by both `parse_template` (compile path) and
/// [`extract_template_exprs`] (validate path). The scanner does not call
/// `CelExpression::compile`; expression compilation is the caller's job.
enum RawSegment<'a> {
    /// A literal slice of the source between expressions. Always non-empty
    /// when emitted.
    Literal(&'a str),
    /// An expression slice between `{{` and `}}`, untrimmed. The leading
    /// `{{` byte offset is reported via [`ScanError`] when scanning fails;
    /// successful segments do not carry it because no consumer needs it.
    Expr { expr_src: &'a str },
}

/// Grammar-only error from [`scan_template_segments`].
#[derive(Debug)]
enum ScanError {
    /// A `{{` opened at `offset` but no matching `}}` was found.
    Unterminated { offset: usize },
    /// A `{{ ... }}` pair at `offset` contained only whitespace.
    EmptyExpression { offset: usize },
}

/// The single byte-level template scanner shared by the compile and validate
/// paths.
///
/// Tracks CEL string-literal state (single and double quoted, with `\` as an
/// escape that consumes the following byte) and curly-brace nesting depth so
/// that `}}` inside a CEL string literal or a CEL map literal is not treated
/// as the closing delimiter. Returns slices into `source`; emits
/// [`RawSegment::Literal`] only for non-empty literal runs.
fn scan_template_segments(source: &str) -> Result<Vec<RawSegment<'_>>, ScanError> {
    let bytes = source.as_bytes();
    let mut segments: Vec<RawSegment<'_>> = Vec::new();
    let mut literal_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            // Flush any pending literal.
            if literal_start < i {
                segments.push(RawSegment::Literal(&source[literal_start..i]));
            }
            let expr_start = i + 2;
            let mut j = expr_start;
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
                return Err(ScanError::Unterminated { offset: i });
            }
            let expr_src = &source[expr_start..j];
            if expr_src.trim().is_empty() {
                return Err(ScanError::EmptyExpression { offset: i });
            }
            segments.push(RawSegment::Expr { expr_src });
            i = j + 2;
            literal_start = i;
        } else {
            // ASCII delimiters (`{`, `}`, `"`, `'`, `\`) cannot appear inside
            // a UTF-8 multi-byte sequence (those bytes are all >= 0x80), so a
            // plain byte advance keeps the literal slice on a UTF-8 boundary.
            i += 1;
        }
    }
    if literal_start < bytes.len() {
        segments.push(RawSegment::Literal(&source[literal_start..]));
    }
    Ok(segments)
}

fn parse_template(source: &str) -> MechResult<Vec<Segment>> {
    let raw = scan_template_segments(source).map_err(|e| match e {
        ScanError::Unterminated { offset } => MechError::TemplateParse {
            source_text: source.to_string(),
            message: format!("unterminated `{{{{` at byte offset {offset}"),
        },
        ScanError::EmptyExpression { offset } => MechError::TemplateParse {
            source_text: source.to_string(),
            message: format!("empty expression at byte offset {offset}"),
        },
    })?;
    let mut segments = Vec::with_capacity(raw.len());
    for seg in raw {
        match seg {
            RawSegment::Literal(s) => segments.push(Segment::Literal(s.to_string())),
            RawSegment::Expr { expr_src } => {
                let trimmed = expr_src.trim();
                let expr = CelExpression::compile(trimmed)?;
                segments.push(Segment::Expr(expr));
            }
        }
    }
    Ok(segments)
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

// ---- CEL AST reference extraction (used by validate) ----------------------

use cel_parser::{Atom, Expression, Member};
use std::collections::BTreeSet;

use crate::validate::{Location, ValidationIssue};

/// References collected from a CEL expression AST.
#[derive(Debug, Default)]
pub struct CollectedRefs {
    /// Top-level identifiers (e.g. `input`, `context`, `blocks`).
    pub top_idents: BTreeSet<String>,
    /// Block references discovered as `blocks.<name>.output.<field?>` or
    /// `block.<name>.<field?>`.
    pub block_refs: Vec<(String, Option<String>)>,
}

/// Walk a parsed CEL expression and collect namespace/block references.
pub fn collect_references(expr: &Expression) -> CollectedRefs {
    let mut out = CollectedRefs::default();
    walk_refs(expr, &mut out);
    out
}

fn walk_refs(expr: &Expression, out: &mut CollectedRefs) {
    match expr {
        Expression::Arithmetic(a, _, b)
        | Expression::Relation(a, _, b)
        | Expression::Or(a, b)
        | Expression::And(a, b) => {
            walk_refs(a, out);
            walk_refs(b, out);
        }
        Expression::Ternary(a, b, c) => {
            walk_refs(a, out);
            walk_refs(b, out);
            walk_refs(c, out);
        }
        Expression::Unary(_, a) => walk_refs(a, out),
        Expression::FunctionCall(_, target, args) => {
            if let Some(t) = target {
                walk_refs(t, out);
            }
            for a in args {
                walk_refs(a, out);
            }
        }
        Expression::List(items) => {
            for it in items {
                walk_refs(it, out);
            }
        }
        Expression::Map(entries) => {
            for (k, v) in entries {
                walk_refs(k, out);
                walk_refs(v, out);
            }
        }
        Expression::Atom(_) => {}
        Expression::Ident(name) => {
            out.top_idents.insert(name.as_ref().clone());
        }
        Expression::Member(_, _) => {
            let chain = flatten_member_chain(expr);
            if let Some((root, segments)) = chain {
                out.top_idents.insert(root.clone());
                if (root == "blocks" || root == "block") && !segments.is_empty() {
                    let target_block = segments[0].clone();
                    let field = if segments.len() >= 2 && segments[1] == "output" {
                        segments.get(2).cloned()
                    } else {
                        None
                    };
                    out.block_refs.push((target_block, field));
                }
            } else {
                walk_member_subexprs(expr, out);
            }
        }
    }
}

fn walk_member_subexprs(expr: &Expression, out: &mut CollectedRefs) {
    if let Expression::Member(inner, member) = expr {
        walk_refs(inner, out);
        if let Member::Index(idx) = member.as_ref() {
            walk_refs(idx, out);
        }
        if let Member::Fields(fields) = member.as_ref() {
            for (_, e) in fields {
                walk_refs(e, out);
            }
        }
    }
}

/// Flatten a chain of `Member::Attribute` or `Member::Index(string literal)`
/// accesses ending in an `Ident`.
/// Returns `Some((root_ident, [seg1, seg2, ...]))` if the entire chain
/// consists of attribute accesses or string-index accesses; `None` otherwise.
/// Treating `x["foo"]` the same as `x.foo` ensures that `blocks["name"].output.bar`
/// is recognised as a block reference just like `blocks.name.output.bar`.
pub fn flatten_member_chain(expr: &Expression) -> Option<(String, Vec<String>)> {
    let mut segments: Vec<String> = Vec::new();
    let mut cur = expr;
    loop {
        match cur {
            Expression::Member(inner, member) => match member.as_ref() {
                Member::Attribute(name) => {
                    segments.push(name.as_ref().clone());
                    cur = inner;
                }
                Member::Index(idx_expr) => {
                    let Expression::Atom(Atom::String(s)) = idx_expr.as_ref() else {
                        return None;
                    };
                    segments.push(s.as_ref().clone());
                    cur = inner;
                }
                _ => return None,
            },
            Expression::Ident(name) => {
                segments.reverse();
                return Some((name.as_ref().clone(), segments));
            }
            _ => return None,
        }
    }
}

/// Extract `{{ ... }}` expression segments from a template string for
/// validation. Parsing errors are appended to `errors`.
///
/// Shares the byte-level scanner with the compile path
/// ([`scan_template_segments`]) so validate-time grammar checks match
/// compile-time exactly: brace-nested expressions like `{{ size({"a": 1}) }}`
/// scan as one expression, and empty `{{  }}` segments are reported as
/// validation issues rather than silently dropped.
///
/// Failure semantics are all-or-nothing: when the scanner returns any
/// `ScanError` (unterminated `{{` or empty `{{  }}`), this function pushes
/// exactly one `ValidationIssue` describing the first offending segment
/// and returns an empty `Vec`, even if earlier segments would have been
/// well-formed. This matches the compile path's all-or-nothing failure
/// (`parse_template` returns one `MechError::TemplateParse`) so callers
/// see the same go/no-go decision at validate time and at compile time.
pub fn extract_template_exprs(
    source: &str,
    loc: &Location,
    errors: &mut Vec<ValidationIssue>,
) -> Vec<String> {
    match scan_template_segments(source) {
        Ok(segments) => segments
            .into_iter()
            .filter_map(|seg| match seg {
                RawSegment::Expr { expr_src } => Some(expr_src.trim().to_string()),
                RawSegment::Literal(_) => None,
            })
            .collect(),
        Err(ScanError::Unterminated { offset }) => {
            errors.push(ValidationIssue::new(
                loc.clone(),
                format!("unterminated `{{{{` at byte offset {offset} in template"),
            ));
            Vec::new()
        }
        Err(ScanError::EmptyExpression { offset }) => {
            errors.push(ValidationIssue::new(
                loc.clone(),
                format!("empty `{{{{  }}}}` expression at byte offset {offset} in template"),
            ));
            Vec::new()
        }
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

    // --- extract_template_exprs (validate path) tests ---
    //
    // After Q-01 the validate-time scanner shares one byte loop with the
    // compile-time path, so these tests pin down that grammar errors caught
    // at compile are also caught at validate, and that brace-nested or
    // string-quoted `}}` is not mistaken for the closing delimiter.

    fn validate_loc() -> Location {
        Location::root(None)
    }

    #[test]
    fn extract_template_exprs_reports_unterminated() {
        let mut errors = Vec::new();
        let out = extract_template_exprs("hello {{input.name", &validate_loc(), &mut errors);
        assert!(out.is_empty(), "expected no exprs returned, got {out:?}");
        assert_eq!(errors.len(), 1, "expected one validation issue");
        assert!(
            errors[0].message.contains("unterminated"),
            "expected unterminated message, got {:?}",
            errors[0].message
        );
    }

    #[test]
    fn extract_template_exprs_reports_empty_expression() {
        // Regression test for C-01: validate path used to silently drop
        // empty `{{  }}` expressions; it must now report them.
        let mut errors = Vec::new();
        let out = extract_template_exprs("bad {{  }} seg", &validate_loc(), &mut errors);
        assert!(out.is_empty(), "expected no exprs returned, got {out:?}");
        assert_eq!(errors.len(), 1, "expected one validation issue");
        assert!(
            errors[0].message.contains("empty"),
            "expected empty-expression message, got {:?}",
            errors[0].message
        );
    }

    #[test]
    fn extract_template_exprs_handles_closing_braces_inside_string_literal() {
        // `{{ "}}" }}` — the inner `}}` is inside a CEL string literal and
        // must not terminate the template expression. Should yield exactly
        // one expression and no errors.
        let mut errors = Vec::new();
        let out = extract_template_exprs("{{ \"}}\" }}", &validate_loc(), &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(out, vec!["\"}}\"".to_string()]);
    }

    #[test]
    fn extract_template_exprs_handles_braces_inside_cel_map_literal() {
        // `{{ size({"a": 1}) }}` — sanity check that brace-nested map
        // literals (separated from `}}` by other tokens) scan as one
        // expression.
        let mut errors = Vec::new();
        let out = extract_template_exprs("{{ size({\"a\": 1}) }}", &validate_loc(), &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(out, vec!["size({\"a\": 1})".to_string()]);
    }

    #[test]
    fn extract_template_exprs_handles_map_literal_adjacent_to_close() {
        // Direct C-01 regression test: `{{ {"x": 1}}}` has the map's
        // closing `}` IMMEDIATELY adjacent to the template's `}}`. Without
        // brace-depth tracking the old validate-time scanner would treat
        // bytes 10..12 (`}}` at positions of map-close + first template-
        // close) as the template terminator and return the malformed
        // expression ` {"x": 1` instead of ` {"x": 1}`. This test fails
        // on the pre-Q-01 scanner and passes on the unified scanner.
        let mut errors = Vec::new();
        let out = extract_template_exprs("{{ {\"x\": 1}}}", &validate_loc(), &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(out, vec!["{\"x\": 1}".to_string()]);
    }

    #[test]
    fn extract_template_exprs_returns_nothing_when_any_segment_is_empty() {
        // Locks in the all-or-nothing semantics of `extract_template_exprs`:
        // when the scanner reports `EmptyExpression`, the validate path
        // returns no expressions at all (matching the compile path's
        // all-or-nothing failure), even if earlier segments would have
        // been valid. Documenting this avoids future drift toward
        // "collect partial results plus errors" that would re-diverge
        // from the compile path.
        let mut errors = Vec::new();
        let out = extract_template_exprs("{{x}} {{}}", &validate_loc(), &mut errors);
        assert!(
            out.is_empty(),
            "expected no exprs returned (all-or-nothing), got {out:?}"
        );
        assert_eq!(errors.len(), 1, "expected one validation issue");
        assert!(
            errors[0].message.contains("empty"),
            "expected empty-expression message, got {:?}",
            errors[0].message
        );
    }

    #[test]
    fn compile_and_validate_scanners_reject_the_same_inputs() {
        // Companion to `compile_and_validate_scanners_agree_on_segmentation`:
        // for templates that should fail, both paths must reject. Locks in
        // error parity (compile -> `MechError::TemplateParse`, validate ->
        // `ValidationIssue`) so that a future change splitting the two
        // paths cannot silently regress validate-time error coverage
        // (the original C-01 defect).
        let cases = [
            "hello {{input.name",   // unterminated
            "a {{ }} b",            // empty expression
            "{{x}} {{}}",           // mixed: valid then empty
            "oops {{ {\"x\": 1} }", // unterminated nested map
        ];
        // Extract the `byte offset N` integer from a message string. Both
        // paths use the same wording, so we tolerate either path's exact
        // template; we only care that the integer matches.
        fn extract_offset(msg: &str) -> usize {
            let (_, tail) = msg
                .split_once("byte offset ")
                .unwrap_or_else(|| panic!("message does not contain `byte offset N`: {msg:?}"));
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits
                .parse()
                .unwrap_or_else(|e| panic!("failed to parse offset from {msg:?}: {e}"))
        }
        for src in cases {
            let compile_err = parse_template(src).expect_err("compile path should reject");
            let compile_msg = match &compile_err {
                MechError::TemplateParse { message, .. } => message.clone(),
                other => panic!("expected TemplateParse for {src:?}, got {other:?}"),
            };
            let mut errors = Vec::new();
            let _ = extract_template_exprs(src, &validate_loc(), &mut errors);
            assert_eq!(
                errors.len(),
                1,
                "validate path should report exactly one issue for {src:?}, got {errors:?}"
            );
            let validate_msg = &errors[0].message;
            assert_eq!(
                extract_offset(&compile_msg),
                extract_offset(validate_msg),
                "compile and validate must report the same byte offset for {src:?}: \
                 compile={compile_msg:?} validate={validate_msg:?}"
            );
        }
    }

    #[test]
    fn template_with_literal_close_brace_is_treated_as_text() {
        // A bare `}` outside any `{{...}}` is just a literal byte; the
        // outer scanner loop must not treat it specially. Locks in this
        // edge case so a future refactor that hoists brace-depth tracking
        // to the outer loop cannot regress it.
        let t = Template::compile("foo } bar {{1 + 1}}").unwrap();
        assert_eq!(t.render(&Namespaces::empty()).unwrap(), "foo } bar 2");
        let mut errors = Vec::new();
        let out = extract_template_exprs("foo } bar {{1 + 1}}", &validate_loc(), &mut errors);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(out, vec!["1 + 1".to_string()]);
    }

    #[test]
    fn compile_and_validate_scanners_agree_on_segmentation() {
        // Locks in the Q-01 unification invariant: for any template that
        // both paths accept, the count and trimmed text of expression
        // segments must agree. If the two paths ever diverge again, this
        // test fails. Inputs cover plain literals, single expressions,
        // mixed literal+expression sequences, brace-nested map literals,
        // and `}}` inside CEL string literals.
        let cases = [
            "plain literal only",
            "{{input.name}}",
            "hello {{input.name}}!",
            "a={{x}} b={{y}} c={{z}}",
            "{{ size({\"a\": 1}) }}",
            "{{ {\"x\": 1}}}",
            "{{ \"}}\" }}",
            "{{'{{'}}",
            "",
            "foo } bar {{x}}",
            "}",
        ];
        for src in cases {
            let raw = scan_template_segments(src)
                .unwrap_or_else(|e| panic!("scanner rejected fixture {src:?}: {e:?}"));
            let scanner_exprs: Vec<String> = raw
                .into_iter()
                .filter_map(|seg| match seg {
                    RawSegment::Expr { expr_src } => Some(expr_src.trim().to_string()),
                    RawSegment::Literal(_) => None,
                })
                .collect();
            let mut errors = Vec::new();
            let validate_exprs = extract_template_exprs(src, &validate_loc(), &mut errors);
            assert!(
                errors.is_empty(),
                "validate path raised errors for {src:?}: {errors:?}"
            );
            assert_eq!(
                validate_exprs, scanner_exprs,
                "validate path disagrees with shared scanner for {src:?}"
            );
        }
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

    #[test]
    fn template_renders_null() {
        let ns = Namespaces::new(
            json!({"val": null}),
            json!({}),
            json!({}),
            json!({}),
            json!({}),
        );
        let t = Template::compile("result={{input.val}}").unwrap();
        assert_eq!(t.render(&ns).unwrap(), "result=null");
    }

    #[test]
    fn template_renders_float() {
        let ns = Namespaces::new(
            json!({"pi": 2.72}),
            json!({}),
            json!({}),
            json!({}),
            json!({}),
        );
        let t = Template::compile("val={{input.pi}}").unwrap();
        assert_eq!(t.render(&ns).unwrap(), "val=2.72");
    }

    #[test]
    fn template_renders_bool() {
        let ns = Namespaces::new(
            json!({"flag": true}),
            json!({}),
            json!({}),
            json!({}),
            json!({}),
        );
        let t = Template::compile("ok={{input.flag}}").unwrap();
        assert_eq!(t.render(&ns).unwrap(), "ok=true");
    }

    #[test]
    fn template_renders_map() {
        let ns = Namespaces::new(
            json!({"obj": {"a": 1}}),
            json!({}),
            json!({}),
            json!({}),
            json!({}),
        );
        let t = Template::compile("data={{input.obj}}").unwrap();
        assert_eq!(t.render(&ns).unwrap(), r#"data={"a":1}"#);
    }

    #[test]
    fn template_three_level_nesting() {
        let ns = Namespaces::new(
            json!({}),
            json!({}),
            json!({}),
            json!({"deep": {"output": {"a": {"b": {"c": "found"}}}}}),
            json!({}),
        );
        let t = Template::compile("val={{blocks.deep.output.a.b.c}}").unwrap();
        assert_eq!(t.render(&ns).unwrap(), "val=found");
    }

    #[test]
    fn template_multibyte_literal() {
        let ns = Namespaces::new(
            json!({"name": "wörld"}),
            json!({}),
            json!({}),
            json!({}),
            json!({}),
        );
        let t = Template::compile("Ärger: {{input.name}} — fertig").unwrap();
        assert_eq!(t.render(&ns).unwrap(), "Ärger: wörld — fertig");
    }

    // --- collect_references / flatten_member_chain tests ---

    fn parse_expr(src: &str) -> Expression {
        cel_parser::parse(src).expect("CEL parse failed")
    }

    #[test]
    fn collect_refs_attribute_chain_records_block_ref() {
        // blocks.my_block.output.field — pure attribute chain
        let expr = parse_expr("blocks.my_block.output.field");
        let refs = collect_references(&expr);
        assert!(refs.top_idents.contains("blocks"));
        assert_eq!(
            refs.block_refs,
            vec![("my_block".to_string(), Some("field".to_string()))]
        );
    }

    #[test]
    fn collect_refs_index_string_chain_records_block_ref() {
        // blocks["my_block"].output.field — bracket string-index access
        let expr = parse_expr("blocks[\"my_block\"].output.field");
        let refs = collect_references(&expr);
        assert!(refs.top_idents.contains("blocks"));
        assert_eq!(
            refs.block_refs,
            vec![("my_block".to_string(), Some("field".to_string()))]
        );
    }

    #[test]
    fn collect_refs_mixed_index_and_attribute_chain_records_block_ref() {
        // blocks["my_block"].output["field"] — bracket for block name, attribute for output, bracket for field
        let expr = parse_expr("blocks[\"my_block\"].output[\"field\"]");
        let refs = collect_references(&expr);
        assert!(refs.top_idents.contains("blocks"));
        assert_eq!(
            refs.block_refs,
            vec![("my_block".to_string(), Some("field".to_string()))]
        );
    }

    #[test]
    fn collect_refs_index_non_string_does_not_record_block_ref() {
        // blocks[0] — integer index must NOT produce a block_ref
        let expr = parse_expr("blocks[0]");
        let refs = collect_references(&expr);
        // top_idents still sees "blocks"
        assert!(refs.top_idents.contains("blocks"));
        // but no block_refs entry
        assert!(refs.block_refs.is_empty());
    }

    #[test]
    fn flatten_member_chain_attribute_only() {
        let expr = parse_expr("a.b.c");
        assert_eq!(
            flatten_member_chain(&expr),
            Some(("a".to_string(), vec!["b".to_string(), "c".to_string()]))
        );
    }

    #[test]
    fn flatten_member_chain_string_index() {
        let expr = parse_expr("a[\"b\"][\"c\"]");
        assert_eq!(
            flatten_member_chain(&expr),
            Some(("a".to_string(), vec!["b".to_string(), "c".to_string()]))
        );
    }

    #[test]
    fn flatten_member_chain_mixed() {
        let expr = parse_expr("a.b[\"c\"]");
        assert_eq!(
            flatten_member_chain(&expr),
            Some(("a".to_string(), vec!["b".to_string(), "c".to_string()]))
        );
    }

    #[test]
    fn flatten_member_chain_int_index_returns_none() {
        let expr = parse_expr("a[0]");
        assert_eq!(flatten_member_chain(&expr), None);
    }
}
