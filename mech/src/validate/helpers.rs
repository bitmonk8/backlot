//! Free helper functions used across validate submodules.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::schema::{AgentConfig, BlockDef, CallBlock, CallSpec, FunctionDef, MechDocument};
use serde_json::Value as JsonValue;

/// Returns `true` if `s` matches the identifier pattern `[a-z][a-z0-9_]*`.
pub(crate) fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Compute the normalized grant set for an agent config. `write` and
/// `network` imply `tools`; a non-empty tools list also implies `tools`.
pub(crate) fn normalized_grants(ac: &AgentConfig) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = ac.grants_list().iter().cloned().collect();
    if set.contains("write") || set.contains("network") {
        set.insert("tools".to_string());
    }
    if !ac.tool_list().is_empty() {
        set.insert("tools".to_string());
    }
    set
}

/// Extract the function names referenced by a call block (all three forms).
pub(crate) fn called_function_names(c: &CallBlock) -> Vec<String> {
    match &c.call {
        CallSpec::Single(name) => vec![name.clone()],
        CallSpec::Uniform(names) => names.clone(),
        CallSpec::PerCall(entries) => entries.iter().map(|e| e.func.clone()).collect(),
    }
}

/// Return the set_context and set_workflow keys written by a block.
pub(crate) fn block_writes(b: &BlockDef) -> (BTreeSet<&str>, BTreeSet<&str>) {
    (
        b.set_context().keys().map(String::as_str).collect(),
        b.set_workflow().keys().map(String::as_str).collect(),
    )
}

/// Infer terminal blocks: those with no outgoing transitions.
pub(crate) fn inferred_terminals(func: &FunctionDef) -> Vec<String> {
    func.blocks
        .iter()
        .filter(|(_, b)| b.transitions().is_empty())
        .map(|(name, _)| name.clone())
        .collect()
}

/// Compute, for each block, the transitive closure of `depends_on`.
pub(crate) fn transitive_depends_on(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in func.blocks.keys() {
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<&str> = func
            .blocks
            .get(name)
            .unwrap()
            .depends_on()
            .iter()
            .map(String::as_str)
            .collect();
        while let Some(n) = stack.pop() {
            if acc.insert(n.to_string())
                && let Some(b) = func.blocks.get(n)
            {
                for d in b.depends_on() {
                    stack.push(d.as_str());
                }
            }
        }
        out.insert(name.clone(), acc);
    }
    out
}

/// Compute, per block, the set of blocks forward-reachable via any chain of
/// `transitions[].goto` edges (excluding the block itself).
pub(crate) fn transitive_ctrl_reach(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in func.blocks.keys() {
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        if let Some(b) = func.blocks.get(name) {
            for t in b.transitions() {
                queue.push_back(t.goto.as_str());
            }
        }
        while let Some(n) = queue.pop_front() {
            if acc.insert(n.to_string())
                && let Some(b) = func.blocks.get(n)
            {
                for t in b.transitions() {
                    if !acc.contains(t.goto.as_str()) {
                        queue.push_back(t.goto.as_str());
                    }
                }
            }
        }
        out.insert(name.clone(), acc);
    }
    out
}

/// Collect the set of declared property names per block from their output
/// schemas. For call blocks, resolves the callee's output schema; for
/// Uniform/PerCall, intersects all callees.
pub(crate) fn collect_block_fields(
    func: &FunctionDef,
    wf: &MechDocument,
) -> BTreeMap<String, BTreeSet<String>> {
    let empty = BTreeMap::new();
    let schemas = wf.workflow.as_ref().map(|w| &w.schemas).unwrap_or(&empty);
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, block) in &func.blocks {
        let fields = match block {
            BlockDef::Prompt(p) => {
                extract_schema_properties(&crate::schema::resolve_schema_value(&p.schema, schemas))
            }
            BlockDef::Call(c) => {
                collect_call_schema_fields(c, wf, schemas, extract_schema_properties)
            }
        };
        out.insert(name.clone(), fields);
    }
    out
}

/// Extract the `required` array from a JSON Schema object.
pub(crate) fn schema_required_fields(schema: &JsonValue) -> BTreeSet<String> {
    schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(JsonValue::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Like [`collect_block_fields`] but returns the `required` set per block.
/// For Uniform/PerCall call blocks, intersects the required sets of all
/// callees (a field is guaranteed-required only if ALL callees require it).
pub(crate) fn collect_block_required_fields(
    func: &FunctionDef,
    wf: &MechDocument,
) -> BTreeMap<String, BTreeSet<String>> {
    let empty = BTreeMap::new();
    let schemas = wf.workflow.as_ref().map(|w| &w.schemas).unwrap_or(&empty);
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, block) in &func.blocks {
        let required = match block {
            BlockDef::Prompt(p) => {
                let sv = crate::schema::resolve_schema_value(&p.schema, schemas);
                match sv {
                    Some(v) => schema_required_fields(&v),
                    None => BTreeSet::new(),
                }
            }
            BlockDef::Call(c) => {
                collect_call_schema_fields(c, wf, schemas, extract_schema_required)
            }
        };
        out.insert(name.clone(), required);
    }
    out
}

/// Helper: given a call block, resolve each callee's output schema and apply
/// `extractor`, intersecting results for Uniform/PerCall.
fn collect_call_schema_fields(
    c: &CallBlock,
    wf: &MechDocument,
    schemas: &BTreeMap<String, JsonValue>,
    extractor: fn(&Option<JsonValue>) -> BTreeSet<String>,
) -> BTreeSet<String> {
    match &c.call {
        CallSpec::Single(fname) => {
            let sv = wf
                .functions
                .get(fname)
                .and_then(|f| f.output.as_ref())
                .map(|s| crate::schema::resolve_schema_value(s, schemas));
            match sv {
                Some(ref inner) => extractor(inner),
                None => BTreeSet::new(),
            }
        }
        CallSpec::Uniform(names) => intersect_callee_fields(names.iter(), wf, schemas, extractor),
        CallSpec::PerCall(entries) => {
            intersect_callee_fields(entries.iter().map(|e| &e.func), wf, schemas, extractor)
        }
    }
}

/// Intersect the schema fields from multiple callees. A field is included
/// only if every callee provides it.
fn intersect_callee_fields<'a>(
    names: impl Iterator<Item = &'a String>,
    wf: &MechDocument,
    schemas: &BTreeMap<String, JsonValue>,
    extractor: fn(&Option<JsonValue>) -> BTreeSet<String>,
) -> BTreeSet<String> {
    let mut result: Option<BTreeSet<String>> = None;
    for fname in names {
        let sv = wf
            .functions
            .get(fname.as_str())
            .and_then(|f| f.output.as_ref())
            .map(|s| crate::schema::resolve_schema_value(s, schemas));
        let fields = match sv {
            Some(ref inner) => extractor(inner),
            None => BTreeSet::new(),
        };
        result = Some(match result {
            None => fields,
            Some(acc) => acc.intersection(&fields).cloned().collect(),
        });
    }
    result.unwrap_or_default()
}

fn extract_schema_properties(schema_value: &Option<JsonValue>) -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    if let Some(JsonValue::Object(obj)) = schema_value
        && let Some(props) = obj.get("properties").and_then(JsonValue::as_object)
    {
        for k in props.keys() {
            fields.insert(k.clone());
        }
    }
    fields
}

fn extract_schema_required(schema_value: &Option<JsonValue>) -> BTreeSet<String> {
    match schema_value {
        Some(v) => schema_required_fields(v),
        None => BTreeSet::new(),
    }
}

/// Collect has()-protected paths from a CEL AST.
pub(crate) fn collect_has_protected_paths(expr: &cel_parser::Expression) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    walk_for_has(expr, &mut out);
    out
}

pub(crate) fn walk_for_has(expr: &cel_parser::Expression, out: &mut BTreeSet<Vec<String>>) {
    use cel_parser::{Expression, Member};
    match expr {
        Expression::FunctionCall(name_expr, target, args) => {
            if let Expression::Ident(name) = name_expr.as_ref() {
                if name.as_ref() == "has" {
                    for arg in args {
                        if let Some((root, attrs)) = crate::cel::flatten_member_chain(arg) {
                            let mut path = vec![root];
                            path.extend(attrs);
                            out.insert(path);
                        }
                    }
                }
            }
            walk_for_has(name_expr, out);
            if let Some(t) = target {
                walk_for_has(t, out);
            }
            for a in args {
                walk_for_has(a, out);
            }
        }
        Expression::Arithmetic(a, _, b)
        | Expression::Relation(a, _, b)
        | Expression::Or(a, b)
        | Expression::And(a, b) => {
            walk_for_has(a, out);
            walk_for_has(b, out);
        }
        Expression::Ternary(a, b, c) => {
            walk_for_has(a, out);
            walk_for_has(b, out);
            walk_for_has(c, out);
        }
        Expression::Unary(_, a) => walk_for_has(a, out),
        Expression::Member(inner, member) => {
            walk_for_has(inner, out);
            if let Member::Index(idx) = member.as_ref() {
                walk_for_has(idx, out);
            }
            if let Member::Fields(fields) = member.as_ref() {
                for (_, e) in fields {
                    walk_for_has(e, out);
                }
            }
        }
        Expression::List(items) => {
            for it in items {
                walk_for_has(it, out);
            }
        }
        Expression::Map(entries) => {
            for (k, v) in entries {
                walk_for_has(k, out);
                walk_for_has(v, out);
            }
        }
        Expression::Atom(_) | Expression::Ident(_) => {}
    }
}

/// Collect all field-access paths from a CEL AST.
pub(crate) fn collect_field_access_paths(expr: &cel_parser::Expression) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    walk_for_field_access(expr, &mut out);
    out
}

pub(crate) fn walk_for_field_access(
    expr: &cel_parser::Expression,
    out: &mut BTreeSet<Vec<String>>,
) {
    use cel_parser::Expression;
    match expr {
        Expression::Member(_, _) => {
            if let Some((root, attrs)) = crate::cel::flatten_member_chain(expr) {
                if !attrs.is_empty() {
                    let mut path = vec![root];
                    path.extend(attrs);
                    out.insert(path);
                }
            }
            walk_member_field_access(expr, out);
        }
        Expression::FunctionCall(name_expr, target, args) => {
            let is_has = matches!(name_expr.as_ref(), Expression::Ident(n) if n.as_ref() == "has");
            walk_for_field_access(name_expr, out);
            if let Some(t) = target {
                walk_for_field_access(t, out);
            }
            if !is_has {
                for a in args {
                    walk_for_field_access(a, out);
                }
            }
        }
        Expression::Arithmetic(a, _, b)
        | Expression::Relation(a, _, b)
        | Expression::Or(a, b)
        | Expression::And(a, b) => {
            walk_for_field_access(a, out);
            walk_for_field_access(b, out);
        }
        Expression::Ternary(a, b, c) => {
            walk_for_field_access(a, out);
            walk_for_field_access(b, out);
            walk_for_field_access(c, out);
        }
        Expression::Unary(_, a) => walk_for_field_access(a, out),
        Expression::List(items) => {
            for it in items {
                walk_for_field_access(it, out);
            }
        }
        Expression::Map(entries) => {
            for (k, v) in entries {
                walk_for_field_access(k, out);
                walk_for_field_access(v, out);
            }
        }
        Expression::Atom(_) | Expression::Ident(_) => {}
    }
}

pub(crate) fn walk_member_field_access(
    expr: &cel_parser::Expression,
    out: &mut BTreeSet<Vec<String>>,
) {
    use cel_parser::{Expression, Member};
    if let Expression::Member(inner, member) = expr {
        match member.as_ref() {
            Member::Attribute(_) => walk_member_field_access(inner, out),
            Member::Index(idx) => {
                walk_for_field_access(inner, out);
                walk_for_field_access(idx, out);
            }
            Member::Fields(fields) => {
                walk_for_field_access(inner, out);
                for (_, e) in fields {
                    walk_for_field_access(e, out);
                }
            }
        }
    }
}

// ---- Constants ------------------------------------------------------------

pub(crate) const RESERVED_BLOCK_NAMES: &[&str] = &["input", "output", "context", "workflow"];
pub(crate) const VALID_GRANTS: &[&str] = &["tools", "write", "network"];
pub(crate) const VALID_JSON_TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "array", "object", "null",
];
pub(crate) const ALLOWED_NAMESPACES: &[&str] =
    &["input", "output", "context", "workflow", "blocks", "block"];
