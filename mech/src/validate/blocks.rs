//! Block-level, transition, and call-target validation methods on [`Validator`].

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;

use crate::schema::{BlockDef, CallSpec, ContextVarDef, FunctionDef, MechDocument, SchemaRef};

use super::Validator;
use super::helpers::{RESERVED_BLOCK_NAMES, inferred_terminals, is_valid_identifier};
use super::model::ModelChecker;
use super::report::Location;

impl Validator<'_> {
    pub(crate) fn validate_function(
        &mut self,
        fn_name: &str,
        func: &FunctionDef,
        wf: &MechDocument,
        function_names: &BTreeSet<String>,
        models: &dyn ModelChecker,
    ) {
        let floc = self.root_loc().with_function(fn_name);

        if !is_valid_identifier(fn_name) {
            self.err(
                floc.clone().with_field("name"),
                format!("function name `{fn_name}` is not a valid identifier ([a-z][a-z0-9_]*)"),
            );
        }

        // Function input schema validity
        self.validate_json_schema_object(&func.input, floc.clone().with_field("input"), false);

        // Function output schema validity (explicit)
        if let Some(output) = &func.output {
            match output {
                SchemaRef::Inline(v) => {
                    self.validate_json_schema_object(v, floc.clone().with_field("output"), false)
                }
                SchemaRef::Ref(raw) => {
                    self.validate_schema_ref_resolves(raw, wf, floc.clone().with_field("output"));
                }
                SchemaRef::Infer => {}
            }
        }

        // Function-level context declarations
        self.validate_context_map(&func.context, &floc.clone().with_field("context"));

        // Function-level agent override
        if let Some(agent_ref) = &func.agent {
            let defaults = wf.workflow.as_ref();
            self.validate_agent_ref(
                agent_ref,
                defaults,
                models,
                floc.clone().with_field("agent"),
            );
        }

        // Block names: format, reserved, uniqueness implicit (BTreeMap)
        for (block_name, block) in &func.blocks {
            let bloc = floc.clone().with_block(block_name);
            if !is_valid_identifier(block_name) {
                self.err(
                    bloc.clone().with_field("name"),
                    format!(
                        "block name `{block_name}` is not a valid identifier ([a-z][a-z0-9_]*)"
                    ),
                );
            }
            if RESERVED_BLOCK_NAMES.contains(&block_name.as_str()) {
                self.err(
                    bloc.clone().with_field("name"),
                    format!("block name `{block_name}` is reserved (conflicts with CEL namespace)"),
                );
            }
            self.validate_block(block, func, wf, function_names, models, &bloc);
        }

        // Terminal validation
        let inferred = inferred_terminals(func);
        let effective_terminals: BTreeSet<String> = if func.terminals.is_empty() {
            inferred.iter().cloned().collect()
        } else {
            for t in &func.terminals {
                if !func.blocks.contains_key(t) {
                    self.err(
                        floc.clone().with_field(format!("terminals[{t}]")),
                        format!("terminal block `{t}` does not exist"),
                    );
                    continue;
                }
                if let Some(b) = func.blocks.get(t) {
                    if !b.transitions().is_empty() {
                        self.err(
                            floc.clone().with_block(t).with_field("transitions"),
                            "terminal block must have no outgoing transitions",
                        );
                    }
                }
            }
            func.terminals.iter().cloned().collect()
        };

        // Function output inference precondition
        let needs_inference = matches!(func.output, None | Some(SchemaRef::Infer));
        if needs_inference && effective_terminals.is_empty() && !func.blocks.is_empty() {
            self.err(
                floc.clone().with_field("output"),
                "no terminal blocks detected; declare an explicit `output` schema or fix terminals",
            );
        }

        // Dataflow cycle detection
        self.detect_dataflow_cycles(func, &floc);

        // Unreachable blocks (warning)
        self.detect_unreachable_blocks(func, &floc);

        // CEL + template-reference resolution and reachability
        self.validate_cel_and_templates(func, wf, &floc);

        // Parallel context-write conflict (warning)
        self.detect_parallel_context_conflicts(func, &floc);
    }

    fn validate_block(
        &mut self,
        block: &BlockDef,
        func: &FunctionDef,
        wf: &MechDocument,
        function_names: &BTreeSet<String>,
        models: &dyn ModelChecker,
        bloc: &Location,
    ) {
        // Shared validation using BlockDef accessors (Issue #4):
        // depends_on, set_context, set_workflow, transitions
        for dep in block.depends_on() {
            if !func.blocks.contains_key(dep) {
                self.err(
                    bloc.clone().with_field("depends_on"),
                    format!("`depends_on: {dep}` references unknown block"),
                );
            }
        }
        for key in block.set_context().keys() {
            if !func.context.contains_key(key) {
                self.err(
                    bloc.clone().with_field(format!("set_context.{key}")),
                    format!("`set_context.{key}` is not declared in the function's `context`"),
                );
            }
        }
        for key in block.set_workflow().keys() {
            let declared = wf
                .workflow
                .as_ref()
                .is_some_and(|d| d.context.contains_key(key));
            if !declared {
                self.err(
                    bloc.clone().with_field(format!("set_workflow.{key}")),
                    format!("`set_workflow.{key}` is not declared in `workflow.context`"),
                );
            }
        }
        self.validate_transitions(block.transitions(), func, bloc);

        // Block-type-specific validation
        match block {
            BlockDef::Prompt(p) => {
                // Schema validity
                match &p.schema {
                    SchemaRef::Inline(v) => {
                        self.validate_json_schema_object(v, bloc.clone().with_field("schema"), true)
                    }
                    SchemaRef::Ref(raw) => {
                        self.validate_schema_ref_resolves(
                            raw,
                            wf,
                            bloc.clone().with_field("schema"),
                        );
                    }
                    SchemaRef::Infer => {
                        self.err(
                            bloc.clone().with_field("schema"),
                            "prompt block schema cannot be `infer`",
                        );
                    }
                }
                // Agent override
                if let Some(agent_ref) = &p.agent {
                    self.validate_agent_ref(
                        agent_ref,
                        wf.workflow.as_ref(),
                        models,
                        bloc.clone().with_field("agent"),
                    );
                }
            }
            BlockDef::Call(c) => {
                let is_per_call = matches!(c.call, CallSpec::PerCall(_));
                if is_per_call && c.input.is_some() {
                    self.err(
                        bloc.clone().with_field("input"),
                        "per-call list block must not have a block-level `input`",
                    );
                }
                if !is_per_call && c.input.is_none() {
                    self.err(
                        bloc.clone().with_field("input"),
                        "single-function or uniform-list call block requires a block-level `input`",
                    );
                }

                use crate::schema::ParallelStrategy;
                if matches!(c.parallel, Some(ParallelStrategy::NOfM)) && c.n.is_none() {
                    self.err(
                        bloc.clone().with_field("n"),
                        "`parallel: n_of_m` requires an `n` field",
                    );
                }
                if c.n.is_some() && !matches!(c.parallel, Some(ParallelStrategy::NOfM)) {
                    self.err(
                        bloc.clone().with_field("n"),
                        "`n` is only valid with `parallel: n_of_m`",
                    );
                }
                if let (Some(n), Some(ParallelStrategy::NOfM)) = (c.n, c.parallel) {
                    let len = match &c.call {
                        CallSpec::Uniform(v) => v.len(),
                        CallSpec::PerCall(v) => v.len(),
                        CallSpec::Single(_) => 1,
                    };
                    if (n as usize) < 1 || (n as usize) > len {
                        self.err(
                            bloc.clone().with_field("n"),
                            format!("`n` must be in 1..={len}, got {n}"),
                        );
                    }
                }

                self.validate_call_targets(c, function_names, wf, bloc);
            }
        }
    }

    fn validate_transitions(
        &mut self,
        transitions: &[crate::schema::TransitionDef],
        func: &FunctionDef,
        bloc: &Location,
    ) {
        let mut seen_unconditional = false;
        for (i, t) in transitions.iter().enumerate() {
            if !func.blocks.contains_key(&t.goto) {
                self.err(
                    bloc.clone().with_field(format!("transitions[{i}].goto")),
                    format!("transition target `{}` does not exist", t.goto),
                );
            }
            if seen_unconditional {
                self.warn(
                    bloc.clone().with_field(format!("transitions[{i}]")),
                    "transition is unreachable: appears after an unconditional fallback",
                );
            }
            if t.when.is_none() {
                seen_unconditional = true;
            }
        }
    }

    fn validate_call_targets(
        &mut self,
        c: &crate::schema::CallBlock,
        function_names: &BTreeSet<String>,
        wf: &MechDocument,
        bloc: &Location,
    ) {
        match &c.call {
            CallSpec::Single(name) => {
                self.validate_call_fn(name, c.input.as_ref(), function_names, wf, bloc);
            }
            CallSpec::Uniform(names) => {
                for name in names {
                    self.validate_call_fn(name, c.input.as_ref(), function_names, wf, bloc);
                }
            }
            CallSpec::PerCall(entries) => {
                for (i, entry) in entries.iter().enumerate() {
                    if entry.func.is_empty() {
                        self.err(
                            bloc.clone().with_field(format!("call[{i}].fn")),
                            "per-call entry missing `fn`",
                        );
                    }
                    self.validate_call_fn(
                        &entry.func,
                        Some(&entry.input),
                        function_names,
                        wf,
                        bloc,
                    );
                }
            }
        }
    }

    /// Issue #48: renamed from `check_call_fn` → `validate_call_fn`.
    fn validate_call_fn(
        &mut self,
        name: &str,
        input: Option<&BTreeMap<String, String>>,
        function_names: &BTreeSet<String>,
        wf: &MechDocument,
        bloc: &Location,
    ) {
        if !function_names.contains(name) {
            self.err(
                bloc.clone().with_field("call"),
                format!("call target `{name}` is not a function in this workflow"),
            );
            return;
        }
        let Some(callee) = wf.functions.get(name) else {
            return;
        };
        let provided: BTreeSet<String> = input
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        if let Some(req) = callee.input.get("required").and_then(JsonValue::as_array) {
            for r in req {
                if let Some(rs) = r.as_str()
                    && !provided.contains(rs)
                {
                    self.err(
                        bloc.clone().with_field("input"),
                        format!("call to `{name}` is missing required input field `{rs}`"),
                    );
                }
            }
        }
    }

    pub(crate) fn validate_json_schema_object(
        &mut self,
        v: &JsonValue,
        loc: Location,
        require_required_nonempty: bool,
    ) {
        if let Err(e) = jsonschema::validator_for(v) {
            self.err(loc.clone(), format!("invalid JSON Schema: {e}"));
            return;
        }
        let Some(obj) = v.as_object() else {
            self.err(loc, "schema must be a JSON object");
            return;
        };
        match obj.get("type").and_then(JsonValue::as_str) {
            Some("object") => {}
            Some(other) => {
                self.err(
                    loc.clone(),
                    format!("schema root type must be `object`, got `{other}`"),
                );
                return;
            }
            None => {
                self.err(loc.clone(), "schema must declare root type `object`");
                return;
            }
        }
        if require_required_nonempty {
            match obj.get("required").and_then(JsonValue::as_array) {
                Some(a) if !a.is_empty() => {}
                _ => self.err(loc, "schema `required` must be present and non-empty"),
            }
        }
    }

    pub(crate) fn validate_schema_ref_resolves(
        &mut self,
        raw: &str,
        wf: &MechDocument,
        loc: Location,
    ) {
        let Some(rest) = raw.strip_prefix("$ref:") else {
            self.err(loc, format!("malformed schema $ref: `{raw}`"));
            return;
        };
        if let Some(name) = rest.strip_prefix('#') {
            if name.is_empty() {
                self.err(loc, format!("malformed schema $ref: `{raw}`"));
                return;
            }
            let exists = wf
                .workflow
                .as_ref()
                .is_some_and(|d| d.schemas.contains_key(name));
            if !exists {
                self.err(
                    loc,
                    format!("schema $ref `#{name}` does not resolve to a workflow-level schema"),
                );
            }
        }
    }

    pub(crate) fn validate_context_map(
        &mut self,
        map: &BTreeMap<String, ContextVarDef>,
        loc: &Location,
    ) {
        for (name, def) in map {
            if !is_valid_identifier(name) {
                self.err(
                    loc.clone().with_field(name.clone()),
                    format!("context variable name `{name}` is not a valid identifier"),
                );
            }
            if !super::helpers::VALID_JSON_TYPES.contains(&def.ty.as_str()) {
                self.err(
                    loc.clone().with_field(format!("{name}.type")),
                    format!(
                        "context variable `{name}` has invalid JSON Schema type `{}`",
                        def.ty
                    ),
                );
                continue;
            }
            if !crate::schema::value_matches_json_type(&def.initial, &def.ty) {
                self.err(
                    loc.clone().with_field(format!("{name}.initial")),
                    format!(
                        "initial value for `{name}` is not compatible with declared type `{}`",
                        def.ty
                    ),
                );
            }
        }
    }
}
