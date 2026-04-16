//! CEL/template validation methods on [`Validator`].

use std::collections::{BTreeMap, BTreeSet};

use crate::cel::{self, CelExpression};
use crate::schema::{BlockDef, CallSpec, FunctionDef, WorkflowFile};

use super::Validator;
use super::graph::compute_dominators;
use super::helpers::{
    ALLOWED_NAMESPACES, called_function_names, collect_block_fields, collect_block_required_fields,
    collect_field_access_paths, collect_has_protected_paths, schema_required_fields,
    transitive_depends_on,
};
use super::report::Location;

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
        wf: &WorkflowFile,
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
        if let Err(e) = CelExpression::compile(expr_src) {
            self.err(loc.clone(), format!("CEL compile error: {e}"));
            return;
        }
        let ast = match cel_parser::parse(expr_src) {
            Ok(a) => a,
            Err(_) => return,
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
