//! CEL/template validation methods on [`Validator`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;

use crate::cel;
use crate::schema::{BlockDef, CallBlock, CallSpec, FunctionDef, MechDocument};

use super::Validator;
use super::graph::{compute_dominators, transitive_depends_on};
use super::helpers::{ALLOWED_NAMESPACES, called_function_names};
use super::report::Location;

// ---- CEL AST walker functions ---------------------------------------------

/// Collect has()-protected paths from a CEL AST.
fn collect_has_protected_paths(expr: &cel_parser::Expression) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    walk_for_has(expr, &mut out);
    out
}

fn walk_for_has(expr: &cel_parser::Expression, out: &mut BTreeSet<Vec<String>>) {
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
fn collect_field_access_paths(expr: &cel_parser::Expression) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    walk_for_field_access(expr, &mut out);
    out
}

fn walk_for_field_access(expr: &cel_parser::Expression, out: &mut BTreeSet<Vec<String>>) {
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

fn walk_member_field_access(expr: &cel_parser::Expression, out: &mut BTreeSet<Vec<String>>) {
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

// ---- Validator methods ----------------------------------------------------

/// Context struct for validate_cel_expr parameters (Issue #4: removes
/// `#[allow(clippy::too_many_arguments)]`).
struct CelCheckCtx<'a> {
    block_fields: &'a BTreeMap<String, BTreeSet<String>>,
    dominators: &'a BTreeMap<String, BTreeSet<String>>,
    dep_closure: &'a BTreeMap<String, BTreeSet<String>>,
    allow_output: bool,
    forbid_blocks: bool,
    extra_allowed_vars: &'a [&'a str],
}

impl Validator<'_> {
    /// Issue #56: removed unused `_fn_name` parameter.
    pub(crate) fn validate_cel_and_templates(
        &mut self,
        func: &FunctionDef,
        wf: &MechDocument,
        floc: &Location,
    ) {
        let block_fields = collect_block_fields(func, wf);
        let dominators = compute_dominators(func);
        let dep_closure = transitive_depends_on(func);
        let block_required_fields = collect_block_required_fields(func, wf);
        let input_required = schema_required_fields(&func.input);

        for (name, block) in &func.blocks {
            let bloc = floc.clone().with_block(name);

            // Shared: set_context, set_workflow, transitions (Issue #4)
            for (k, expr) in block.set_context() {
                let field_loc = bloc.clone().with_field(format!("set_context.{k}"));
                self.validate_cel_expr(
                    expr,
                    &field_loc,
                    name,
                    &CelCheckCtx {
                        block_fields: &block_fields,
                        dominators: &dominators,
                        dep_closure: &dep_closure,
                        allow_output: true,
                        forbid_blocks: false,
                        extra_allowed_vars: &[],
                    },
                );
                self.validate_cel_optional_field_safety(
                    expr,
                    &field_loc,
                    name,
                    &block_required_fields,
                    &input_required,
                );
            }
            for (k, expr) in block.set_workflow() {
                let field_loc = bloc.clone().with_field(format!("set_workflow.{k}"));
                self.validate_cel_expr(
                    expr,
                    &field_loc,
                    name,
                    &CelCheckCtx {
                        block_fields: &block_fields,
                        dominators: &dominators,
                        dep_closure: &dep_closure,
                        allow_output: true,
                        forbid_blocks: false,
                        extra_allowed_vars: &[],
                    },
                );
                self.validate_cel_optional_field_safety(
                    expr,
                    &field_loc,
                    name,
                    &block_required_fields,
                    &input_required,
                );
            }
            for (i, t) in block.transitions().iter().enumerate() {
                if let Some(when) = &t.when {
                    let field_loc = bloc.clone().with_field(format!("transitions[{i}].when"));
                    self.validate_cel_expr(
                        when,
                        &field_loc,
                        name,
                        &CelCheckCtx {
                            block_fields: &block_fields,
                            dominators: &dominators,
                            dep_closure: &dep_closure,
                            allow_output: true,
                            forbid_blocks: true,
                            extra_allowed_vars: &[],
                        },
                    );
                    self.validate_cel_optional_field_safety(
                        when,
                        &field_loc,
                        name,
                        &block_required_fields,
                        &input_required,
                    );
                }
            }

            // Block-type-specific template validation
            match block {
                BlockDef::Prompt(p) => {
                    self.validate_template(
                        &p.prompt,
                        &bloc.clone().with_field("prompt"),
                        name,
                        &CelCheckCtx {
                            block_fields: &block_fields,
                            dominators: &dominators,
                            dep_closure: &dep_closure,
                            allow_output: false,
                            forbid_blocks: false,
                            extra_allowed_vars: &[],
                        },
                    );
                }
                BlockDef::Call(c) => {
                    if let Some(input) = &c.input {
                        for (k, expr) in input {
                            self.validate_template(
                                expr,
                                &bloc.clone().with_field(format!("input.{k}")),
                                name,
                                &CelCheckCtx {
                                    block_fields: &block_fields,
                                    dominators: &dominators,
                                    dep_closure: &dep_closure,
                                    allow_output: false,
                                    forbid_blocks: false,
                                    extra_allowed_vars: &[],
                                },
                            );
                        }
                    }
                    if let CallSpec::PerCall(entries) = &c.call {
                        for (i, e) in entries.iter().enumerate() {
                            for (k, expr) in &e.input {
                                self.validate_template(
                                    expr,
                                    &bloc.clone().with_field(format!("call[{i}].input.{k}")),
                                    name,
                                    &CelCheckCtx {
                                        block_fields: &block_fields,
                                        dominators: &dominators,
                                        dep_closure: &dep_closure,
                                        allow_output: false,
                                        forbid_blocks: false,
                                        extra_allowed_vars: &[],
                                    },
                                );
                            }
                        }
                    }
                    if let Some(output) = &c.output {
                        let called_fn_names = called_function_names(c);
                        let extra_refs: Vec<&str> =
                            called_fn_names.iter().map(String::as_str).collect();
                        for (k, expr) in output {
                            self.validate_template(
                                expr,
                                &bloc.clone().with_field(format!("output.{k}")),
                                name,
                                &CelCheckCtx {
                                    block_fields: &block_fields,
                                    dominators: &dominators,
                                    dep_closure: &dep_closure,
                                    allow_output: false,
                                    forbid_blocks: false,
                                    extra_allowed_vars: &extra_refs,
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    /// Issue #48: renamed from `check_template` → `validate_template`.
    fn validate_template(
        &mut self,
        source: &str,
        loc: &Location,
        cur_block: &str,
        ctx: &CelCheckCtx<'_>,
    ) {
        for expr_src in cel::extract_template_exprs(source, loc, &mut self.report.errors) {
            self.validate_cel_expr(&expr_src, loc, cur_block, ctx);
        }
    }

    /// Issue #48: renamed from `check_cel_expr` → `validate_cel_expr`.
    fn validate_cel_expr(
        &mut self,
        expr_src: &str,
        loc: &Location,
        cur_block: &str,
        ctx: &CelCheckCtx<'_>,
    ) {
        // Single parse — cel_parser::parse is the same parse that
        // CelExpression::compile calls internally.
        let ast = match cel_parser::parse(expr_src) {
            Ok(a) => a,
            Err(e) => {
                self.err(loc.clone(), format!("CEL compile error: {e}"));
                return;
            }
        };
        let refs = cel::collect_references(&ast);

        // Variable scope check.
        for v in &refs.top_idents {
            if !ALLOWED_NAMESPACES.contains(&v.as_str())
                && !ctx.extra_allowed_vars.contains(&v.as_str())
            {
                let mut allowed: Vec<&str> = ALLOWED_NAMESPACES.to_vec();
                allowed.extend_from_slice(ctx.extra_allowed_vars);
                self.err(
                    loc.clone(),
                    format!(
                        "CEL expression references unknown variable `{v}`; allowed: {}",
                        allowed.join(", ")
                    ),
                );
            }
            if v == "output" && !ctx.allow_output {
                self.err(
                    loc.clone(),
                    "`output` is not in scope here (only available in set_context, set_workflow, transitions)",
                );
            }
            if (v == "blocks" || v == "block") && ctx.forbid_blocks {
                self.err(
                    loc.clone(),
                    "`blocks` is not in scope inside transition `when` guards",
                );
            }
        }

        // Block reference resolution + reachability (Issue #55: removed
        // Option wrapper from block_refs).
        for (target_block, field) in &refs.block_refs {
            if ctx.forbid_blocks {
                continue;
            }
            let Some(fields) = ctx.block_fields.get(target_block) else {
                self.err(
                    loc.clone(),
                    format!("template references unknown block `blocks.{target_block}`"),
                );
                continue;
            };
            if let Some(f) = field
                && !fields.is_empty()
                && !fields.contains(f)
            {
                self.err(
                    loc.clone(),
                    format!("template references unknown field `blocks.{target_block}.output.{f}`"),
                );
            }
            let dominates = ctx
                .dominators
                .get(cur_block)
                .is_some_and(|s| s.contains(target_block));
            let in_deps = ctx
                .dep_closure
                .get(cur_block)
                .is_some_and(|s| s.contains(target_block));
            if !dominates && !in_deps && target_block != cur_block {
                self.err(
                    loc.clone(),
                    format!(
                        "template reference to `blocks.{target_block}` is not statically reachable: \
                         add `depends_on: [{target_block}]` or ensure it dominates `{cur_block}`"
                    ),
                );
            }
        }
    }

    /// Issue #48: renamed from `check_cel_optional_field_safety` →
    /// `validate_cel_optional_field_safety`.
    fn validate_cel_optional_field_safety(
        &mut self,
        expr_src: &str,
        loc: &Location,
        cur_block: &str,
        block_required_fields: &BTreeMap<String, BTreeSet<String>>,
        input_required: &BTreeSet<String>,
    ) {
        let ast = match cel_parser::parse(expr_src) {
            Ok(a) => a,
            Err(_) => return,
        };

        let protected = collect_has_protected_paths(&ast);
        let accesses = collect_field_access_paths(&ast);

        for path in &accesses {
            if path.len() < 2 {
                continue;
            }
            let namespace = &path[0];

            if namespace == "context" || namespace == "workflow" {
                continue;
            }

            let (field_name, required_set) = if namespace == "output" {
                let field = &path[1];
                let req = block_required_fields.get(cur_block);
                (field.as_str(), req)
            } else if namespace == "input" {
                let field = &path[1];
                (field.as_str(), Some(input_required))
            } else if namespace == "blocks" || namespace == "block" {
                if path.len() >= 4 && path[2] == "output" {
                    let block_name = &path[1];
                    let field = &path[3];
                    let req = block_required_fields.get(block_name.as_str());
                    (field.as_str(), req)
                } else {
                    continue;
                }
            } else {
                continue;
            };

            let Some(req_set) = required_set else {
                continue;
            };

            if req_set.contains(field_name) {
                continue;
            }

            let is_protected = protected.iter().any(|hp| {
                hp.len() <= path.len() && hp.iter().zip(path.iter()).all(|(a, b)| a == b)
            });

            if !is_protected {
                let path_str = path.join(".");
                self.err(
                    loc.clone(),
                    format!(
                        "CEL optional field safety: `{path_str}` accesses field `{field_name}` \
                         which is not in `required` — add `has({path_str})` guard"
                    ),
                );
            }
        }
    }
}

// ---- Schema field collection (moved from helpers.rs) ----------------------

/// Collect the set of declared property names per block from their output
/// schemas. For call blocks, resolves the callee's output schema; for
/// Uniform/PerCall, intersects all callees.
fn collect_block_fields(
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
fn schema_required_fields(schema: &JsonValue) -> BTreeSet<String> {
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
fn collect_block_required_fields(
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
