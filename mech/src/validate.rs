//! Load-time validation of a parsed [`WorkflowFile`] (spec §10.1).
//!
//! # Ordering invariant
//!
//! This pass runs **before** function output inference. Do not read
//! resolved/inferred output schemas here — functions declaring `output: infer`
//! still have an unresolved schema at this point, and any check that peeks at
//! a concrete output shape would silently skip them. See `loader.rs`
//! `load_impl` for the ordering contract and how to add a post-inference pass
//! if one becomes necessary.
//!
//! [`validate_workflow`] walks the parsed YAML AST and emits the **complete**
//! list of errors and warnings — it never short-circuits on the first error.
//! All checks listed in `docs/MECH_SPEC.md` §10.1 are implemented here:
//! structural (block discrimination, name format, context declarations, …),
//! graph (DAG check on `depends_on`, transition target existence, dominator-
//! based template reachability, …), and type (schema validity, CEL
//! compilation + variable scope, CEL optional field safety, agent model resolution, input-schema match
//! against callee, …).
//!
//! # Hermetic agent model resolution
//!
//! The spec says `agent.model` resolves via flick's `ModelRegistry`. That is
//! a filesystem-touching operation, so this module accepts any
//! [`ModelChecker`] implementation. Two ready-made impls are provided:
//!
//! * [`AnyModel`] — accepts every model name (use in tests where model
//!   resolution is irrelevant).
//! * [`KnownModels`] — accepts only names from a fixed set.
//!
//! Production callers should pass an adapter over flick's `ModelRegistry`.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use cel_parser::{Expression, Member};
use serde_json::Value as JsonValue;

use crate::cel::CelExpression;
use crate::schema::{
    AgentConfig, AgentConfigRef, BlockDef, CallBlock, CallSpec, ContextVarDef, FunctionDef,
    InferLiteral, SchemaRef, TransitionDef, WorkflowFile,
};

// ---- Public API -----------------------------------------------------------

/// Trait for resolving an agent model name to "exists" / "does not exist".
///
/// Production code should wrap flick's `ModelRegistry`. Tests typically use
/// [`AnyModel`] or [`KnownModels`] to avoid filesystem access.
pub trait ModelChecker {
    /// Returns `true` if the named model is known.
    fn knows(&self, model: &str) -> bool;
}

/// [`ModelChecker`] that accepts every model name. Useful for tests where
/// model resolution is not under test.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnyModel;

impl ModelChecker for AnyModel {
    fn knows(&self, _model: &str) -> bool {
        true
    }
}

/// [`ModelChecker`] backed by a fixed set of known model names.
#[derive(Debug, Clone, Default)]
pub struct KnownModels {
    names: HashSet<String>,
}

impl KnownModels {
    /// Build from any iterator of names.
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }
}

impl ModelChecker for KnownModels {
    fn knows(&self, model: &str) -> bool {
        self.names.contains(model)
    }
}

/// Source location for a validation issue. All fields are optional.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Location {
    /// Workflow file path (passed in by the caller).
    pub file: Option<PathBuf>,
    /// Function name within the workflow.
    pub function: Option<String>,
    /// Block name within the function.
    pub block: Option<String>,
    /// Field name within the block (e.g. `prompt`, `transitions[0].when`).
    pub field: Option<String>,
}

impl Location {
    fn root(file: Option<&Path>) -> Self {
        Self {
            file: file.map(Path::to_path_buf),
            ..Self::default()
        }
    }

    fn with_function(mut self, name: &str) -> Self {
        self.function = Some(name.to_string());
        self
    }

    fn with_block(mut self, name: &str) -> Self {
        self.block = Some(name.to_string());
        self
    }

    fn with_field(mut self, name: impl Into<String>) -> Self {
        self.field = Some(name.into());
        self
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = &self.file {
            parts.push(p.display().to_string());
        }
        if let Some(fun) = &self.function {
            parts.push(format!("function `{fun}`"));
        }
        if let Some(b) = &self.block {
            parts.push(format!("block `{b}`"));
        }
        if let Some(field) = &self.field {
            parts.push(format!("field `{field}`"));
        }
        if parts.is_empty() {
            f.write_str("<workflow>")
        } else {
            f.write_str(&parts.join(" / "))
        }
    }
}

/// A single validation finding (error or warning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Where in the workflow the issue was found.
    pub location: Location,
    /// Human-readable description of the problem.
    pub message: String,
}

impl ValidationIssue {
    fn new(location: Location, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

/// Result of validating a workflow file.
#[derive(Debug, Default, Clone)]
pub struct ValidationReport {
    /// Errors prevent execution.
    pub errors: Vec<ValidationIssue>,
    /// Warnings are informational and do not prevent execution.
    pub warnings: Vec<ValidationIssue>,
}

impl ValidationReport {
    /// True iff there are no errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a parsed workflow against the §10.1 checklist.
///
/// `file_path` is folded into the source location of every emitted issue.
/// `models` is consulted for `agent.model` resolution.
pub fn validate_workflow(
    workflow: &WorkflowFile,
    file_path: Option<&Path>,
    models: &dyn ModelChecker,
) -> ValidationReport {
    let mut v = Validator::new(file_path);
    v.run(workflow, models);
    v.report
}

// ---- Internal validator state --------------------------------------------

struct Validator<'a> {
    file: Option<&'a Path>,
    report: ValidationReport,
}

const RESERVED_BLOCK_NAMES: &[&str] = &["input", "output", "context", "workflow"];
const VALID_GRANTS: &[&str] = &["tools", "write", "network"];
const VALID_JSON_TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "array", "object", "null",
];
const ALLOWED_NAMESPACES: &[&str] = &["input", "output", "context", "workflow", "blocks", "block"];

impl<'a> Validator<'a> {
    fn new(file: Option<&'a Path>) -> Self {
        Self {
            file,
            report: ValidationReport::default(),
        }
    }

    fn err(&mut self, loc: Location, msg: impl Into<String>) {
        self.report.errors.push(ValidationIssue::new(loc, msg));
    }

    fn warn(&mut self, loc: Location, msg: impl Into<String>) {
        self.report.warnings.push(ValidationIssue::new(loc, msg));
    }

    fn root_loc(&self) -> Location {
        Location::root(self.file)
    }

    fn run(&mut self, wf: &WorkflowFile, models: &dyn ModelChecker) {
        // Top-level
        if wf.functions.is_empty() {
            self.err(
                self.root_loc().with_field("functions"),
                "workflow must declare at least one function",
            );
        }

        // Workflow-level context
        if let Some(defaults) = &wf.workflow {
            self.validate_context_map(
                &defaults.context,
                &self.root_loc().with_field("workflow.context"),
            );
            // Workflow-level agents map: cycle/extends/grant validity
            self.validate_named_agents(defaults, models);
            // Workflow-level default agent
            if let Some(agent_ref) = &defaults.agent {
                self.validate_agent_ref(
                    agent_ref,
                    defaults,
                    models,
                    self.root_loc().with_field("workflow.agent"),
                );
            }
        }

        // Function-level
        let function_names: BTreeSet<String> = wf.functions.keys().cloned().collect();
        for (fn_name, func) in &wf.functions {
            self.validate_function(fn_name, func, wf, &function_names, models);
        }
    }

    // ---- Functions --------------------------------------------------------

    fn validate_function(
        &mut self,
        fn_name: &str,
        func: &FunctionDef,
        wf: &WorkflowFile,
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
        self.validate_json_schema_object(
            &func.input,
            floc.clone().with_field("input"),
            /*require_required_nonempty=*/ false,
        );

        // Function output schema validity (explicit)
        if let Some(output) = &func.output {
            match output {
                SchemaRef::Inline(v) => {
                    self.validate_json_schema_object(v, floc.clone().with_field("output"), false)
                }
                SchemaRef::Ref(raw) => {
                    self.validate_schema_ref_resolves(raw, wf, floc.clone().with_field("output"));
                }
                SchemaRef::Infer(InferLiteral::Infer) => {}
            }
        }

        // Function-level context declarations
        self.validate_context_map(&func.context, &floc.clone().with_field("context"));

        // Function-level agent override
        if let Some(agent_ref) = &func.agent {
            let defaults = wf.workflow.as_ref();
            self.validate_agent_ref_with_defaults(
                agent_ref,
                defaults,
                models,
                floc.clone().with_field("agent"),
            );
        }

        // Block names: format, reserved, uniqueness implicit (BTreeMap), and field validity
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
            self.validate_block(block_name, block, func, wf, function_names, models, &bloc);
        }

        // Terminal validation
        let inferred_terminals = inferred_terminals(func);
        let effective_terminals: BTreeSet<String> = if func.terminals.is_empty() {
            inferred_terminals.iter().cloned().collect()
        } else {
            for t in &func.terminals {
                if !func.blocks.contains_key(t) {
                    self.err(
                        floc.clone().with_field(format!("terminals[{t}]")),
                        format!("terminal block `{t}` does not exist"),
                    );
                    continue;
                }
                // Must have no outgoing transitions
                if let Some(BlockDef::Prompt(p)) = func.blocks.get(t) {
                    if !p.transitions.is_empty() {
                        self.err(
                            floc.clone().with_block(t).with_field("transitions"),
                            "terminal block must have no outgoing transitions",
                        );
                    }
                } else if let Some(BlockDef::Call(c)) = func.blocks.get(t) {
                    if !c.transitions.is_empty() {
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
        let needs_inference = matches!(
            func.output,
            None | Some(SchemaRef::Infer(InferLiteral::Infer))
        );
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
        self.validate_cel_and_templates(fn_name, func, wf, &floc);

        // Parallel context-write conflict (warning) — best-effort: detect any
        // pair of dataflow-parallel blocks (no transitive depends_on either
        // way) that write the same set_context/set_workflow key.
        self.detect_parallel_context_conflicts(func, &floc);
    }

    // ---- Block-level checks ----------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn validate_block(
        &mut self,
        _block_name: &str,
        block: &BlockDef,
        func: &FunctionDef,
        wf: &WorkflowFile,
        function_names: &BTreeSet<String>,
        models: &dyn ModelChecker,
        bloc: &Location,
    ) {
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
                    SchemaRef::Infer(_) => {
                        self.err(
                            bloc.clone().with_field("schema"),
                            "prompt block schema cannot be `infer`",
                        );
                    }
                }
                // depends_on targets exist
                for dep in &p.depends_on {
                    if !func.blocks.contains_key(dep) {
                        self.err(
                            bloc.clone().with_field("depends_on"),
                            format!("`depends_on: {dep}` references unknown block"),
                        );
                    }
                }
                // set_context / set_workflow target validity
                for key in p.set_context.keys() {
                    if !func.context.contains_key(key) {
                        self.err(
                            bloc.clone().with_field(format!("set_context.{key}")),
                            format!(
                                "`set_context.{key}` is not declared in the function's `context`"
                            ),
                        );
                    }
                }
                for key in p.set_workflow.keys() {
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
                // Transitions: target existence + dead-after-fallback
                self.validate_transitions(&p.transitions, func, bloc);
                // Agent override
                if let Some(agent_ref) = &p.agent {
                    self.validate_agent_ref_with_defaults(
                        agent_ref,
                        wf.workflow.as_ref(),
                        models,
                        bloc.clone().with_field("agent"),
                    );
                }
            }
            BlockDef::Call(c) => {
                // Per-call list consistency + input/output rules
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

                // n_of_m needs n; n needs n_of_m; n in range
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

                // Call target existence + per-call entry validity + input schema match
                self.validate_call_targets(c, function_names, wf, bloc);

                // depends_on
                for dep in &c.depends_on {
                    if !func.blocks.contains_key(dep) {
                        self.err(
                            bloc.clone().with_field("depends_on"),
                            format!("`depends_on: {dep}` references unknown block"),
                        );
                    }
                }
                // set_context / set_workflow target validity
                for key in c.set_context.keys() {
                    if !func.context.contains_key(key) {
                        self.err(
                            bloc.clone().with_field(format!("set_context.{key}")),
                            format!(
                                "`set_context.{key}` is not declared in the function's `context`"
                            ),
                        );
                    }
                }
                for key in c.set_workflow.keys() {
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
                self.validate_transitions(&c.transitions, func, bloc);
            }
        }
    }

    fn validate_transitions(
        &mut self,
        transitions: &[TransitionDef],
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
        wf: &WorkflowFile,
        bloc: &Location,
    ) {
        match &c.call {
            CallSpec::Single(name) => {
                self.check_call_fn(name, c.input.as_ref(), function_names, wf, bloc);
            }
            CallSpec::Uniform(names) => {
                for name in names {
                    self.check_call_fn(name, c.input.as_ref(), function_names, wf, bloc);
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
                    self.check_call_fn(&entry.func, Some(&entry.input), function_names, wf, bloc);
                }
            }
        }
    }

    fn check_call_fn(
        &mut self,
        name: &str,
        input: Option<&BTreeMap<String, String>>,
        function_names: &BTreeSet<String>,
        wf: &WorkflowFile,
        bloc: &Location,
    ) {
        if !function_names.contains(name) {
            self.err(
                bloc.clone().with_field("call"),
                format!("call target `{name}` is not a function in this workflow"),
            );
            return;
        }
        // Input schema match: provided keys must include callee's required fields.
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

    // ---- Schemas / context ------------------------------------------------

    fn validate_json_schema_object(
        &mut self,
        v: &JsonValue,
        loc: Location,
        require_required_nonempty: bool,
    ) {
        // Compile via jsonschema crate as a sanity check.
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

    fn validate_schema_ref_resolves(&mut self, raw: &str, wf: &WorkflowFile, loc: Location) {
        // Only `$ref:#name` (workflow-level shared schemas) is checked here;
        // `$ref:path` (external file) is left to Deliverable 7's loader.
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
        // External path refs are not checked here.
    }

    fn validate_context_map(&mut self, map: &BTreeMap<String, ContextVarDef>, loc: &Location) {
        for (name, def) in map {
            if !is_valid_identifier(name) {
                self.err(
                    loc.clone().with_field(name.clone()),
                    format!("context variable name `{name}` is not a valid identifier"),
                );
            }
            if !VALID_JSON_TYPES.contains(&def.ty.as_str()) {
                self.err(
                    loc.clone().with_field(format!("{name}.type")),
                    format!(
                        "context variable `{name}` has invalid JSON Schema type `{}`",
                        def.ty
                    ),
                );
                continue;
            }
            if !value_matches_json_type(&def.initial, &def.ty) {
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

    // ---- Agent configs ---------------------------------------------------

    fn validate_named_agents(
        &mut self,
        defaults: &crate::schema::WorkflowDefaults,
        models: &dyn ModelChecker,
    ) {
        // Per-agent validity
        for (name, ac) in &defaults.agents {
            let loc = self
                .root_loc()
                .with_field(format!("workflow.agents.{name}"));
            self.validate_agent_inline(ac, models, loc);
        }
        // Cycle detection on extends chains.
        for name in defaults.agents.keys() {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut cur = Some(name.clone());
            while let Some(c) = cur {
                if !seen.insert(c.clone()) {
                    self.err(
                        self.root_loc()
                            .with_field(format!("workflow.agents.{name}.extends")),
                        format!("cyclic `extends` chain involving agent `{c}`"),
                    );
                    break;
                }
                cur = defaults
                    .agents
                    .get(&c)
                    .and_then(|a| a.extends.clone())
                    .filter(|n| {
                        if !defaults.agents.contains_key(n) {
                            self.report.errors.push(ValidationIssue::new(
                                Location::root(self.file)
                                    .with_field(format!("workflow.agents.{c}.extends")),
                                format!("`extends` target `{n}` is not a named agent"),
                            ));
                            false
                        } else {
                            true
                        }
                    });
            }
        }
    }

    fn validate_agent_inline(
        &mut self,
        ac: &AgentConfig,
        models: &dyn ModelChecker,
        loc: Location,
    ) {
        if let Some(model) = &ac.model
            && !model.is_empty()
            && !models.knows(model)
        {
            self.err(
                loc.clone().with_field("model"),
                format!("agent model `{model}` is not known to the model registry"),
            );
        }
        for g in ac.grant_list() {
            if !VALID_GRANTS.contains(&g.as_str()) {
                self.err(
                    loc.clone().with_field("grant"),
                    format!("invalid grant `{g}`; must be one of tools/write/network"),
                );
            }
        }
        // Normalization warning: write_paths without write grant.
        let normalized = normalized_grants(ac);
        if !ac.write_path_list().is_empty() && !normalized.contains("write") {
            self.warn(
                loc.with_field("write_paths"),
                "`write_paths` is set but `write` grant is not present (write_paths will be ignored)",
            );
        }
    }

    fn validate_agent_ref(
        &mut self,
        agent_ref: &AgentConfigRef,
        defaults: &crate::schema::WorkflowDefaults,
        models: &dyn ModelChecker,
        loc: Location,
    ) {
        match agent_ref {
            AgentConfigRef::Inline(ac) => {
                if let Some(parent) = &ac.extends
                    && !defaults.agents.contains_key(parent)
                {
                    self.err(
                        loc.clone().with_field("extends"),
                        format!("`extends` target `{parent}` is not a named agent"),
                    );
                }
                self.validate_agent_inline(ac, models, loc);
            }
            AgentConfigRef::Ref(raw) => {
                let Some(rest) = raw.strip_prefix("$ref:") else {
                    self.err(loc, format!("malformed agent $ref: `{raw}`"));
                    return;
                };
                if let Some(name) = rest.strip_prefix('#') {
                    if name.is_empty() || !defaults.agents.contains_key(name) {
                        self.err(
                            loc,
                            format!(
                                "agent $ref `#{name}` is not a named agent in `workflow.agents`"
                            ),
                        );
                    }
                }
                // External file refs (`$ref:path`) deferred to loader (D7).
            }
        }
    }

    fn validate_agent_ref_with_defaults(
        &mut self,
        agent_ref: &AgentConfigRef,
        defaults: Option<&crate::schema::WorkflowDefaults>,
        models: &dyn ModelChecker,
        loc: Location,
    ) {
        if let Some(d) = defaults {
            self.validate_agent_ref(agent_ref, d, models, loc);
        } else {
            // No workflow defaults at all — only inline configs are valid.
            match agent_ref {
                AgentConfigRef::Inline(ac) => {
                    if let Some(parent) = &ac.extends {
                        self.err(
                            loc.clone().with_field("extends"),
                            format!(
                                "`extends` target `{parent}` is not a named agent (no `workflow.agents` declared)"
                            ),
                        );
                    }
                    self.validate_agent_inline(ac, models, loc);
                }
                AgentConfigRef::Ref(raw) => {
                    self.err(
                        loc,
                        format!("agent $ref `{raw}` cannot resolve: no `workflow.agents` declared"),
                    );
                }
            }
        }
    }

    // ---- Graph: dataflow cycle, unreachable, parallel conflicts ----------

    fn detect_dataflow_cycles(&mut self, func: &FunctionDef, floc: &Location) {
        // 0=white, 1=gray, 2=black
        let mut color: BTreeMap<&str, u8> = func.blocks.keys().map(|k| (k.as_str(), 0u8)).collect();
        for start in func.blocks.keys() {
            if color[start.as_str()] != 0 {
                continue;
            }
            let mut stack: Vec<(&str, usize)> = vec![(start.as_str(), 0)];
            color.insert(start.as_str(), 1);
            while let Some(&(node, idx)) = stack.last() {
                let deps = block_depends_on(func.blocks.get(node).unwrap());
                if idx < deps.len() {
                    let next = deps[idx].as_str();
                    let last_idx = stack.len() - 1;
                    stack[last_idx].1 += 1;
                    match color.get(next).copied() {
                        Some(0) => {
                            color.insert(next, 1);
                            stack.push((next, 0));
                        }
                        Some(1) => {
                            self.err(
                                floc.clone().with_block(node).with_field("depends_on"),
                                format!(
                                    "dataflow cycle: `{node}` -> `{next}` closes a cycle in `depends_on`"
                                ),
                            );
                        }
                        _ => {}
                    }
                } else {
                    color.insert(node, 2);
                    stack.pop();
                }
            }
        }
    }

    fn detect_unreachable_blocks(&mut self, func: &FunctionDef, floc: &Location) {
        // Build inbound counts (control + data).
        // `depends_on` means "this block depends on X", so the dataflow edge
        // runs X -> self (X must execute before self). For inbound counting,
        // a block B with `depends_on = [dep]` contributes an edge dep -> B,
        // which increments B's inbound count. Transitions (`goto: X`) add an
        // inbound edge to X as usual.
        let mut inbound: BTreeMap<&str, usize> =
            func.blocks.keys().map(|k| (k.as_str(), 0usize)).collect();
        // Precompute reverse-depends_on adjacency: dep -> list of blocks that
        // depend on dep (i.e., forward dataflow successors of dep).
        let mut rev_deps: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (name, block) in &func.blocks {
            for t in block_transitions(block) {
                if let Some(c) = inbound.get_mut(t.goto.as_str()) {
                    *c += 1;
                }
            }
            for d in block_depends_on(block) {
                if func.blocks.contains_key(d) {
                    if let Some(c) = inbound.get_mut(name.as_str()) {
                        *c += 1;
                    }
                    if let Some((k, _)) = func.blocks.get_key_value(d) {
                        rev_deps.entry(k.as_str()).or_default().push(name.as_str());
                    }
                }
            }
        }
        // Entry points = blocks with zero inbound edges (no control predecessor
        // and no block depends on them as a prerequisite). Reachability: BFS
        // forward from every zero-inbound block, following control edges
        // (`transitions[].goto`) and forward dataflow edges (reverse of
        // `depends_on`).
        let mut reachable: BTreeSet<&str> = BTreeSet::new();
        let entries: Vec<&str> = inbound
            .iter()
            .filter(|(_, c)| **c == 0)
            .map(|(k, _)| *k)
            .collect();
        let mut queue: VecDeque<&str> = entries.iter().copied().collect();
        for e in &entries {
            reachable.insert(e);
        }
        while let Some(node) = queue.pop_front() {
            let block = func.blocks.get(node).unwrap();
            for t in block_transitions(block) {
                if reachable.insert(t.goto.as_str()) {
                    if let Some((k, _)) = func.blocks.get_key_value(&t.goto) {
                        queue.push_back(k.as_str());
                    }
                }
            }
            if let Some(succs) = rev_deps.get(node) {
                for s in succs {
                    if reachable.insert(*s) {
                        queue.push_back(*s);
                    }
                }
            }
        }
        for name in func.blocks.keys() {
            if !reachable.contains(name.as_str()) {
                self.warn(
                    floc.clone().with_block(name),
                    "block is unreachable from any entry point",
                );
            }
        }
    }

    fn detect_parallel_context_conflicts(&mut self, func: &FunctionDef, floc: &Location) {
        // Compute transitive depends_on closure on the dataflow DAG.
        let names: Vec<&str> = func.blocks.keys().map(String::as_str).collect();
        let closure = transitive_depends_on(func);
        let ctrl_reach = transitive_ctrl_reach(func);
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                let a = names[i];
                let b = names[j];
                let a_dep_b = closure.get(a).is_some_and(|s| s.contains(b));
                let b_dep_a = closure.get(b).is_some_and(|s| s.contains(a));
                if a_dep_b || b_dep_a {
                    continue;
                }
                let a_ctrl_b = ctrl_reach.get(a).is_some_and(|s| s.contains(b));
                let b_ctrl_a = ctrl_reach.get(b).is_some_and(|s| s.contains(a));
                if a_ctrl_b || b_ctrl_a {
                    continue;
                }
                let (sa_ctx, sa_wf) = block_writes(func.blocks.get(a).unwrap());
                let (sb_ctx, sb_wf) = block_writes(func.blocks.get(b).unwrap());
                let ctx_overlap: BTreeSet<&str> = sa_ctx.intersection(&sb_ctx).copied().collect();
                let wf_overlap: BTreeSet<&str> = sa_wf.intersection(&sb_wf).copied().collect();
                for k in ctx_overlap {
                    self.warn(
                        floc.clone(),
                        format!(
                            "blocks `{a}` and `{b}` may run in parallel and both write `set_context.{k}`"
                        ),
                    );
                }
                for k in wf_overlap {
                    self.warn(
                        floc.clone(),
                        format!(
                            "blocks `{a}` and `{b}` may run in parallel and both write `set_workflow.{k}`"
                        ),
                    );
                }
            }
        }
    }

    // ---- CEL + template references ---------------------------------------

    fn validate_cel_and_templates(
        &mut self,
        _fn_name: &str,
        func: &FunctionDef,
        wf: &WorkflowFile,
        floc: &Location,
    ) {
        // Pre-collect schema field maps for each block (for `blocks.X.output.Y`
        // existence checking).
        let block_fields = collect_block_fields(func, wf);
        let dominators = compute_dominators(func);
        let dep_closure = transitive_depends_on(func);
        let block_required_fields = collect_block_required_fields(func, wf);
        let input_required = schema_required_fields(&func.input);

        for (name, block) in &func.blocks {
            let bloc = floc.clone().with_block(name);
            // Templates: prompts (prompt blocks) and string values in input/output (call blocks)
            match block {
                BlockDef::Prompt(p) => {
                    self.check_template(
                        &p.prompt,
                        &bloc.clone().with_field("prompt"),
                        name,
                        &block_fields,
                        &dominators,
                        &dep_closure,
                        false,
                    );
                    for (k, expr) in &p.set_context {
                        let field_loc = bloc.clone().with_field(format!("set_context.{k}"));
                        self.check_cel_expr(
                            expr,
                            &field_loc,
                            name,
                            &block_fields,
                            &dominators,
                            &dep_closure,
                            true,
                            false,
                            &[],
                        );
                        self.check_cel_optional_field_safety(
                            expr,
                            &field_loc,
                            name,
                            &block_required_fields,
                            &input_required,
                        );
                    }
                    for (k, expr) in &p.set_workflow {
                        let field_loc = bloc.clone().with_field(format!("set_workflow.{k}"));
                        self.check_cel_expr(
                            expr,
                            &field_loc,
                            name,
                            &block_fields,
                            &dominators,
                            &dep_closure,
                            true,
                            false,
                            &[],
                        );
                        self.check_cel_optional_field_safety(
                            expr,
                            &field_loc,
                            name,
                            &block_required_fields,
                            &input_required,
                        );
                    }
                    for (i, t) in p.transitions.iter().enumerate() {
                        if let Some(when) = &t.when {
                            let field_loc =
                                bloc.clone().with_field(format!("transitions[{i}].when"));
                            self.check_cel_expr(
                                when,
                                &field_loc,
                                name,
                                &block_fields,
                                &dominators,
                                &dep_closure,
                                true,
                                /*forbid_blocks=*/ true,
                                &[],
                            );
                            self.check_cel_optional_field_safety(
                                when,
                                &field_loc,
                                name,
                                &block_required_fields,
                                &input_required,
                            );
                        }
                    }
                }
                BlockDef::Call(c) => {
                    if let Some(input) = &c.input {
                        for (k, expr) in input {
                            self.check_template(
                                expr,
                                &bloc.clone().with_field(format!("input.{k}")),
                                name,
                                &block_fields,
                                &dominators,
                                &dep_closure,
                                false,
                            );
                        }
                    }
                    if let CallSpec::PerCall(entries) = &c.call {
                        for (i, e) in entries.iter().enumerate() {
                            for (k, expr) in &e.input {
                                self.check_template(
                                    expr,
                                    &bloc.clone().with_field(format!("call[{i}].input.{k}")),
                                    name,
                                    &block_fields,
                                    &dominators,
                                    &dep_closure,
                                    false,
                                );
                            }
                        }
                    }
                    if let Some(output) = &c.output {
                        // Call block output mappings can reference called
                        // function names as top-level variables
                        // (`<fn_name>.output.*` per spec §4.4).
                        let called_fn_names = called_function_names(c);
                        let extra_refs: Vec<&str> =
                            called_fn_names.iter().map(String::as_str).collect();
                        for (k, expr) in output {
                            self.check_template_with_extras(
                                expr,
                                &bloc.clone().with_field(format!("output.{k}")),
                                name,
                                &block_fields,
                                &dominators,
                                &dep_closure,
                                false,
                                &extra_refs,
                            );
                        }
                    }
                    for (k, expr) in &c.set_context {
                        let field_loc = bloc.clone().with_field(format!("set_context.{k}"));
                        self.check_cel_expr(
                            expr,
                            &field_loc,
                            name,
                            &block_fields,
                            &dominators,
                            &dep_closure,
                            true,
                            false,
                            &[],
                        );
                        self.check_cel_optional_field_safety(
                            expr,
                            &field_loc,
                            name,
                            &block_required_fields,
                            &input_required,
                        );
                    }
                    for (k, expr) in &c.set_workflow {
                        let field_loc = bloc.clone().with_field(format!("set_workflow.{k}"));
                        self.check_cel_expr(
                            expr,
                            &field_loc,
                            name,
                            &block_fields,
                            &dominators,
                            &dep_closure,
                            true,
                            false,
                            &[],
                        );
                        self.check_cel_optional_field_safety(
                            expr,
                            &field_loc,
                            name,
                            &block_required_fields,
                            &input_required,
                        );
                    }
                    for (i, t) in c.transitions.iter().enumerate() {
                        if let Some(when) = &t.when {
                            let field_loc =
                                bloc.clone().with_field(format!("transitions[{i}].when"));
                            self.check_cel_expr(
                                when,
                                &field_loc,
                                name,
                                &block_fields,
                                &dominators,
                                &dep_closure,
                                true,
                                /*forbid_blocks=*/ true,
                                &[],
                            );
                            self.check_cel_optional_field_safety(
                                when,
                                &field_loc,
                                name,
                                &block_required_fields,
                                &input_required,
                            );
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_template(
        &mut self,
        source: &str,
        loc: &Location,
        cur_block: &str,
        block_fields: &BTreeMap<String, BTreeSet<String>>,
        dominators: &BTreeMap<String, BTreeSet<String>>,
        dep_closure: &BTreeMap<String, BTreeSet<String>>,
        forbid_blocks: bool,
    ) {
        self.check_template_with_extras(
            source,
            loc,
            cur_block,
            block_fields,
            dominators,
            dep_closure,
            forbid_blocks,
            &[],
        );
    }

    /// Like [`check_template`] but accepts additional top-level variable names
    /// that the CEL expressions may reference. Used for call block output
    /// mappings where function names are valid top-level identifiers.
    #[allow(clippy::too_many_arguments)]
    fn check_template_with_extras(
        &mut self,
        source: &str,
        loc: &Location,
        cur_block: &str,
        block_fields: &BTreeMap<String, BTreeSet<String>>,
        dominators: &BTreeMap<String, BTreeSet<String>>,
        dep_closure: &BTreeMap<String, BTreeSet<String>>,
        forbid_blocks: bool,
        extra_allowed_vars: &[&str],
    ) {
        // Extract `{{ ... }}` segments and check each as a CEL expression.
        for expr_src in extract_template_exprs(source, loc, &mut self.report.errors) {
            self.check_cel_expr(
                &expr_src,
                loc,
                cur_block,
                block_fields,
                dominators,
                dep_closure,
                /*allow_output=*/ false,
                forbid_blocks,
                extra_allowed_vars,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_cel_expr(
        &mut self,
        expr_src: &str,
        loc: &Location,
        cur_block: &str,
        block_fields: &BTreeMap<String, BTreeSet<String>>,
        dominators: &BTreeMap<String, BTreeSet<String>>,
        dep_closure: &BTreeMap<String, BTreeSet<String>>,
        allow_output: bool,
        forbid_blocks: bool,
        extra_allowed_vars: &[&str],
    ) {
        // Compile.
        if let Err(e) = CelExpression::compile(expr_src) {
            self.err(loc.clone(), format!("CEL compile error: {e}"));
            return;
        }
        // Parse via cel-parser to walk the AST.
        let ast = match cel_parser::parse(expr_src) {
            Ok(a) => a,
            Err(_) => return, // already reported above
        };
        let refs = collect_references(&ast);

        // Variable scope check.
        for v in &refs.top_idents {
            if !ALLOWED_NAMESPACES.contains(&v.as_str())
                && !extra_allowed_vars.contains(&v.as_str())
            {
                let mut allowed: Vec<&str> = ALLOWED_NAMESPACES.to_vec();
                allowed.extend_from_slice(extra_allowed_vars);
                self.err(
                    loc.clone(),
                    format!(
                        "CEL expression references unknown variable `{v}`; allowed: {}",
                        allowed.join(", ")
                    ),
                );
            }
            if v == "output" && !allow_output {
                self.err(
                    loc.clone(),
                    "`output` is not in scope here (only available in set_context, set_workflow, transitions)",
                );
            }
            if (v == "blocks" || v == "block") && forbid_blocks {
                self.err(
                    loc.clone(),
                    "`blocks` is not in scope inside transition `when` guards",
                );
            }
        }

        // Block reference resolution + reachability.
        for bref in &refs.block_refs {
            // bref is (name, Option<field>)
            if forbid_blocks {
                // already reported
                continue;
            }
            let Some((target_block, field)) = bref else {
                continue;
            };
            let Some(fields) = block_fields.get(target_block) else {
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
            // Reachability: target must dominate cur_block OR be in its
            // transitive depends_on closure.
            let dominates = dominators
                .get(cur_block)
                .is_some_and(|s| s.contains(target_block));
            let in_deps = dep_closure
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

    // ---- CEL optional field safety (MECH_SPEC §10.1) ----------------------

    fn check_cel_optional_field_safety(
        &mut self,
        expr_src: &str,
        loc: &Location,
        cur_block: &str,
        block_required_fields: &BTreeMap<String, BTreeSet<String>>,
        input_required: &BTreeSet<String>,
    ) {
        let ast = match cel_parser::parse(expr_src) {
            Ok(a) => a,
            Err(_) => return, // parse error already reported by check_cel_expr
        };

        let protected = collect_has_protected_paths(&ast);
        let accesses = collect_field_access_paths(&ast);

        for path in &accesses {
            if path.len() < 2 {
                continue;
            }
            let namespace = &path[0];

            // context and workflow namespaces: all fields are effectively
            // required (initialized with `initial` values) — skip.
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

            // Field is optional (not in required). Check if protected by has().
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

// ---- Helpers --------------------------------------------------------------

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn value_matches_json_type(v: &JsonValue, ty: &str) -> bool {
    match ty {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => false,
    }
}

fn normalized_grants(ac: &AgentConfig) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = ac.grant_list().iter().cloned().collect();
    if set.contains("write") || set.contains("network") {
        set.insert("tools".to_string());
    }
    if !ac.tool_list().is_empty() {
        set.insert("tools".to_string());
    }
    set
}

/// Extract the function names referenced by a call block (all three forms).
fn called_function_names(c: &CallBlock) -> Vec<String> {
    match &c.call {
        CallSpec::Single(name) => vec![name.clone()],
        CallSpec::Uniform(names) => names.clone(),
        CallSpec::PerCall(entries) => entries.iter().map(|e| e.func.clone()).collect(),
    }
}

fn block_depends_on(b: &BlockDef) -> &[String] {
    match b {
        BlockDef::Prompt(p) => &p.depends_on,
        BlockDef::Call(c) => &c.depends_on,
    }
}

fn block_transitions(b: &BlockDef) -> &[TransitionDef] {
    match b {
        BlockDef::Prompt(p) => &p.transitions,
        BlockDef::Call(c) => &c.transitions,
    }
}

fn block_writes(b: &BlockDef) -> (BTreeSet<&str>, BTreeSet<&str>) {
    match b {
        BlockDef::Prompt(p) => (
            p.set_context.keys().map(String::as_str).collect(),
            p.set_workflow.keys().map(String::as_str).collect(),
        ),
        BlockDef::Call(c) => (
            c.set_context.keys().map(String::as_str).collect(),
            c.set_workflow.keys().map(String::as_str).collect(),
        ),
    }
}

fn inferred_terminals(func: &FunctionDef) -> Vec<String> {
    // A block is an inferred terminal if it has no outgoing control edges
    // (transitions). Per MECH_SPEC §10.1 and §7, terminals are defined by the
    // control graph only; a control-flow terminal may still be read as a data
    // dependency by other blocks.
    func.blocks
        .iter()
        .filter(|(_, b)| block_transitions(b).is_empty())
        .map(|(name, _)| name.clone())
        .collect()
}

fn transitive_depends_on(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in func.blocks.keys() {
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<&str> = block_depends_on(func.blocks.get(name).unwrap())
            .iter()
            .map(String::as_str)
            .collect();
        while let Some(n) = stack.pop() {
            if acc.insert(n.to_string())
                && let Some(b) = func.blocks.get(n)
            {
                for d in block_depends_on(b) {
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
fn transitive_ctrl_reach(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in func.blocks.keys() {
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        if let Some(b) = func.blocks.get(name) {
            for t in block_transitions(b) {
                queue.push_back(t.goto.as_str());
            }
        }
        while let Some(n) = queue.pop_front() {
            if acc.insert(n.to_string())
                && let Some(b) = func.blocks.get(n)
            {
                for t in block_transitions(b) {
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

/// Compute the dominator set for each block in a function's control-flow
/// graph (transitions only). Inbound-zero blocks are treated as entry points.
fn compute_dominators(func: &FunctionDef) -> BTreeMap<String, BTreeSet<String>> {
    let names: Vec<String> = func.blocks.keys().cloned().collect();
    let all: BTreeSet<String> = names.iter().cloned().collect();

    // Predecessors via control edges only.
    let mut preds: BTreeMap<String, BTreeSet<String>> =
        names.iter().map(|n| (n.clone(), BTreeSet::new())).collect();
    for (src, b) in &func.blocks {
        for t in block_transitions(b) {
            if let Some(e) = preds.get_mut(&t.goto) {
                e.insert(src.clone());
            }
        }
    }
    // Entry = blocks with no inbound control edges.
    let entries: BTreeSet<String> = preds
        .iter()
        .filter(|(_, p)| p.is_empty())
        .map(|(k, _)| k.clone())
        .collect();

    let mut dom: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in &names {
        if entries.contains(n) {
            let mut s = BTreeSet::new();
            s.insert(n.clone());
            dom.insert(n.clone(), s);
        } else {
            dom.insert(n.clone(), all.clone());
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for n in &names {
            if entries.contains(n) {
                continue;
            }
            // Intersect dom of all preds, plus self.
            let p = &preds[n];
            let mut new_set: Option<BTreeSet<String>> = None;
            for pn in p {
                let d = &dom[pn];
                new_set = Some(match new_set {
                    None => d.clone(),
                    Some(acc) => acc.intersection(d).cloned().collect(),
                });
            }
            let mut new_set = new_set.unwrap_or_default();
            new_set.insert(n.clone());
            if new_set != dom[n] {
                dom.insert(n.clone(), new_set);
                changed = true;
            }
        }
    }

    // Post-process: nodes unreachable from any entry should only be dominated
    // by themselves (otherwise the initial `all_blocks` sentinel would make
    // them appear dominated by every block, masking reachability errors in
    // dead code).
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = entries.iter().cloned().collect();
    while let Some(n) = queue.pop_front() {
        if !visited.insert(n.clone()) {
            continue;
        }
        if let Some(b) = func.blocks.get(&n) {
            for t in block_transitions(b) {
                if !visited.contains(t.goto.as_str()) {
                    queue.push_back(t.goto.clone());
                }
            }
        }
    }
    for n in &names {
        if !visited.contains(n) {
            let mut s = BTreeSet::new();
            s.insert(n.clone());
            dom.insert(n.clone(), s);
        }
    }
    dom
}

fn collect_block_fields(
    func: &FunctionDef,
    wf: &WorkflowFile,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, block) in &func.blocks {
        let schema_value = match block {
            BlockDef::Prompt(p) => Some(resolve_schema_value(&p.schema, wf)),
            BlockDef::Call(c) => {
                // Use the called function's output schema, if known and inline.
                match &c.call {
                    CallSpec::Single(fname) => wf
                        .functions
                        .get(fname)
                        .and_then(|f| f.output.as_ref())
                        .map(|s| resolve_schema_value(s, wf)),
                    _ => None,
                }
            }
        };
        let mut fields: BTreeSet<String> = BTreeSet::new();
        if let Some(Some(JsonValue::Object(obj))) = schema_value
            && let Some(props) = obj.get("properties").and_then(JsonValue::as_object)
        {
            for k in props.keys() {
                fields.insert(k.clone());
            }
        }
        out.insert(name.clone(), fields);
    }
    out
}

fn resolve_schema_value(s: &SchemaRef, wf: &WorkflowFile) -> Option<JsonValue> {
    match s {
        SchemaRef::Inline(v) => Some(v.clone()),
        SchemaRef::Ref(raw) => {
            let rest = raw.strip_prefix("$ref:")?;
            let name = rest.strip_prefix('#')?;
            wf.workflow.as_ref()?.schemas.get(name).cloned()
        }
        SchemaRef::Infer(_) => None,
    }
}

// ---- Schema required-field helpers ----------------------------------------

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
fn collect_block_required_fields(
    func: &FunctionDef,
    wf: &WorkflowFile,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, block) in &func.blocks {
        let schema_value = match block {
            BlockDef::Prompt(p) => Some(resolve_schema_value(&p.schema, wf)),
            BlockDef::Call(c) => match &c.call {
                CallSpec::Single(fname) => wf
                    .functions
                    .get(fname)
                    .and_then(|f| f.output.as_ref())
                    .map(|s| resolve_schema_value(s, wf)),
                _ => None,
            },
        };
        let required = match schema_value {
            Some(Some(ref v)) => schema_required_fields(v),
            _ => BTreeSet::new(),
        };
        out.insert(name.clone(), required);
    }
    out
}

// ---- CEL AST walking for has()-protected paths and field accesses ---------

fn collect_has_protected_paths(expr: &Expression) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    walk_for_has(expr, &mut out);
    out
}

fn walk_for_has(expr: &Expression, out: &mut BTreeSet<Vec<String>>) {
    match expr {
        Expression::FunctionCall(name_expr, target, args) => {
            if let Expression::Ident(name) = name_expr.as_ref() {
                if name.as_ref() == "has" {
                    for arg in args {
                        if let Some((root, attrs)) = flatten_member_chain(arg) {
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

fn collect_field_access_paths(expr: &Expression) -> BTreeSet<Vec<String>> {
    let mut out = BTreeSet::new();
    walk_for_field_access(expr, &mut out);
    out
}

fn walk_for_field_access(expr: &Expression, out: &mut BTreeSet<Vec<String>>) {
    match expr {
        Expression::Member(_, _) => {
            if let Some((root, attrs)) = flatten_member_chain(expr) {
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

fn walk_member_field_access(expr: &Expression, out: &mut BTreeSet<Vec<String>>) {
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

// ---- Template scanning ----------------------------------------------------

fn extract_template_exprs(
    source: &str,
    loc: &Location,
    errors: &mut Vec<ValidationIssue>,
) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut j = start;
            let mut quote: Option<u8> = None;
            let mut found = false;
            while j + 1 < bytes.len() {
                let b = bytes[j];
                if let Some(q) = quote {
                    if b == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    if b == q {
                        quote = None;
                    }
                    j += 1;
                    continue;
                }
                if b == b'"' || b == b'\'' {
                    quote = Some(b);
                    j += 1;
                    continue;
                }
                if b == b'}' && bytes[j + 1] == b'}' {
                    found = true;
                    break;
                }
                j += 1;
            }
            if !found {
                errors.push(ValidationIssue::new(
                    loc.clone(),
                    "unterminated `{{` in template",
                ));
                return out;
            }
            let trimmed = source[start..j].trim().to_string();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
            i = j + 2;
        } else {
            i += 1;
        }
    }
    out
}

// ---- AST walking for variable + block reference extraction ---------------

#[derive(Debug, Default)]
struct CollectedRefs {
    /// Top-level identifiers (e.g. `input`, `context`, `blocks`).
    top_idents: BTreeSet<String>,
    /// Block references discovered as `blocks.<name>.output.<field?>` or
    /// `block.<name>.<field?>`. Field is `None` if no further attribute was
    /// chained after `output`.
    block_refs: Vec<Option<(String, Option<String>)>>,
}

fn collect_references(expr: &Expression) -> CollectedRefs {
    let mut out = CollectedRefs::default();
    walk(expr, &mut out);
    out
}

fn walk(expr: &Expression, out: &mut CollectedRefs) {
    match expr {
        Expression::Arithmetic(a, _, b)
        | Expression::Relation(a, _, b)
        | Expression::Or(a, b)
        | Expression::And(a, b) => {
            walk(a, out);
            walk(b, out);
        }
        Expression::Ternary(a, b, c) => {
            walk(a, out);
            walk(b, out);
            walk(c, out);
        }
        Expression::Unary(_, a) => walk(a, out),
        Expression::FunctionCall(_, target, args) => {
            if let Some(t) = target {
                walk(t, out);
            }
            for a in args {
                walk(a, out);
            }
        }
        Expression::List(items) => {
            for it in items {
                walk(it, out);
            }
        }
        Expression::Map(entries) => {
            for (k, v) in entries {
                walk(k, out);
                walk(v, out);
            }
        }
        Expression::Atom(_) => {}
        Expression::Ident(name) => {
            out.top_idents.insert(name.as_ref().clone());
        }
        Expression::Member(_, _) => {
            // Walk the chain. Try to interpret as `blocks.<name>.output.<field?>`
            // or `block.<name>.<field?>`. Otherwise, recurse normally.
            let chain = flatten_member_chain(expr);
            if let Some((root, attrs)) = chain {
                out.top_idents.insert(root.clone());
                if (root == "blocks" || root == "block") && !attrs.is_empty() {
                    let target_block = attrs[0].clone();
                    // Both `blocks.NAME.output.FIELD` and `block.NAME.output.FIELD`
                    // are valid (the `output` wrapper is always present at runtime).
                    let field = if attrs.len() >= 2 && attrs[1] == "output" {
                        attrs.get(2).cloned()
                    } else {
                        // No `output` segment — still record block existence.
                        None
                    };
                    out.block_refs.push(Some((target_block, field)));
                }
            } else {
                // Could not flatten (index/method etc.). Walk subexpressions.
                walk_member_subexprs(expr, out);
            }
        }
    }
}

fn walk_member_subexprs(expr: &Expression, out: &mut CollectedRefs) {
    if let Expression::Member(inner, member) = expr {
        walk(inner, out);
        if let Member::Index(idx) = member.as_ref() {
            walk(idx, out);
        }
        if let Member::Fields(fields) = member.as_ref() {
            for (_, e) in fields {
                walk(e, out);
            }
        }
    }
}

/// Flatten a chain of `Member::Attribute` accesses ending in an `Ident`.
/// Returns `Some((root_ident, [attr1, attr2, ...]))` if the entire chain is
/// attribute access; `None` if any non-attribute member is encountered.
fn flatten_member_chain(expr: &Expression) -> Option<(String, Vec<String>)> {
    let mut attrs: Vec<String> = Vec::new();
    let mut cur = expr;
    loop {
        match cur {
            Expression::Member(inner, member) => match member.as_ref() {
                Member::Attribute(name) => {
                    attrs.push(name.as_ref().clone());
                    cur = inner;
                }
                _ => return None,
            },
            Expression::Ident(name) => {
                attrs.reverse();
                return Some((name.as_ref().clone(), attrs));
            }
            _ => return None,
        }
    }
}

// ---- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_workflow;

    fn ok(yaml: &str) -> ValidationReport {
        let wf = parse_workflow(yaml).expect("yaml parses");
        validate_workflow(&wf, Some(Path::new("test.yaml")), &AnyModel)
    }

    fn run_with(yaml: &str, models: &dyn ModelChecker) -> ValidationReport {
        let wf = parse_workflow(yaml).expect("yaml parses");
        validate_workflow(&wf, Some(Path::new("test.yaml")), models)
    }

    fn assert_clean(r: &ValidationReport) {
        assert!(r.is_ok(), "expected no errors, got: {:#?}", r.errors);
    }

    fn assert_err_contains(r: &ValidationReport, needle: &str) {
        assert!(
            r.errors.iter().any(|e| e.message.contains(needle)),
            "no error contained `{needle}`; errors: {:#?}",
            r.errors
        );
    }

    // ---- Empty / structural ----

    #[test]
    fn rejects_empty_functions() {
        let yaml = "functions: {}\n";
        let r = ok(yaml);
        assert_err_contains(&r, "at least one function");
    }

    #[test]
    fn passes_minimal_workflow() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      done:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        assert_clean(&ok(yaml));
    }

    // ---- Block name format / reserved ----

    #[test]
    fn rejects_invalid_block_name() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      BadName:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "is not a valid identifier");
    }

    #[test]
    fn rejects_reserved_block_name() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      input:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "reserved");
    }

    // ---- Schema validity ----

    #[test]
    fn rejects_schema_root_not_object() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: array
          items: { type: string }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "root type must be `object`");
    }

    #[test]
    fn rejects_schema_empty_required() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          properties: { x: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "required");
    }

    #[test]
    fn schema_ref_resolves() {
        let yaml = r#"
workflow:
  schemas:
    res:
      type: object
      required: [ok]
      properties: { ok: { type: boolean } }
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: "$ref:#res"
"#;
        assert_clean(&ok(yaml));
    }

    #[test]
    fn schema_ref_unresolved() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: "$ref:#missing"
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "does not resolve");
    }

    // ---- Context declarations ----

    #[test]
    fn context_var_invalid_type() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      x: { type: bogus, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "invalid JSON Schema type");
    }

    #[test]
    fn context_var_initial_type_mismatch() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      x: { type: integer, initial: "nope" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "not compatible with declared type");
    }

    #[test]
    fn set_context_target_must_be_declared() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        set_context:
          missing: "1"
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "set_context.missing");
    }

    #[test]
    fn set_workflow_target_must_be_declared() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        set_workflow:
          counter: "1"
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "set_workflow.counter");
    }

    // ---- Transitions ----

    #[test]
    fn transition_target_must_exist() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [a]
          properties: { a: { type: string } }
        transitions:
          - goto: nowhere
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "does not exist");
    }

    #[test]
    fn dead_transitions_after_fallback_warns() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - goto: b
          - goto: b
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert_clean(&r);
        assert!(!r.warnings.is_empty(), "expected dead-transition warning");
    }

    // ---- Dataflow cycle ----

    #[test]
    fn dataflow_cycle_detected() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [b]
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "dataflow cycle");
    }

    // ---- CEL compilation + variable scope ----

    #[test]
    fn guard_compiles_and_scope_ok() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "context.n > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        assert_clean(&ok(yaml));
    }

    #[test]
    fn guard_invalid_cel_errors() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "1 +"
            goto: a
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "CEL compile error");
    }

    #[test]
    fn guard_forbids_blocks_namespace() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "blocks.a.output.k == 'x'"
            goto: a
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "not in scope inside transition `when` guards");
    }

    #[test]
    fn template_unknown_namespace_errors() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "{{junk.x}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "unknown variable");
    }

    // ---- Template reference resolution + reachability ----

    #[test]
    fn template_block_ref_unknown_block() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "{{blocks.nope.output.k}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "unknown block");
    }

    #[test]
    fn template_block_ref_unknown_field() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "{{blocks.a.output.zzz}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "unknown field");
    }

    #[test]
    fn template_block_ref_unreachable() {
        // b references a, but neither dominates nor depends_on a.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "{{blocks.a.output.k}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "not statically reachable");
    }

    #[test]
    fn template_block_ref_via_depends_on_ok() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
      b:
        prompt: "{{blocks.a.output.k}}"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [a]
"#;
        assert_clean(&ok(yaml));
    }

    // ---- Call blocks ----

    #[test]
    fn call_target_must_exist() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: nowhere
        input: { x: "y" }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "is not a function");
    }

    #[test]
    fn call_input_required_field_missing() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: g
        input: {}
  g:
    input:
      type: object
      required: [text]
      properties: { text: { type: string } }
    blocks:
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "missing required input field `text`");
    }

    #[test]
    fn per_call_list_must_not_have_block_input() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call:
          - fn: g
            input: { text: "x" }
        input: { text: "x" }
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      done:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "must not have a block-level `input`");
    }

    #[test]
    fn n_of_m_requires_n() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [g, h]
        input: { text: "x" }
        parallel: n_of_m
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "requires an `n`");
    }

    #[test]
    fn n_out_of_range() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call: [g, h]
        input: { text: "x" }
        parallel: n_of_m
        n: 5
  g:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  h:
    input: { type: object, required: [text], properties: { text: { type: string } } }
    blocks:
      d:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "must be in 1..=2");
    }

    // ---- Terminals ----

    #[test]
    fn explicit_terminal_must_exist() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [nope]
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "terminal block `nope`");
    }

    #[test]
    fn explicit_terminal_must_have_no_outgoing_transitions() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [a]
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: b
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "must have no outgoing transitions");
    }

    // ---- Function output inference precondition ----

    #[test]
    fn no_terminal_blocks_with_infer_errors() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: a
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "no terminal blocks detected");
    }

    // ---- Agent checks ----

    #[test]
    fn agent_unknown_grant_errors() {
        let yaml = r#"
workflow:
  agents:
    a:
      grant: [bogus]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "invalid grant");
    }

    #[test]
    fn agent_unknown_model_errors() {
        let yaml = r#"
workflow:
  agents:
    a:
      model: nonesuch
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let known = KnownModels::new(["sonnet".to_string()]);
        let r = run_with(yaml, &known);
        assert_err_contains(&r, "is not known to the model registry");
    }

    #[test]
    fn agent_extends_unknown_errors() {
        let yaml = r#"
workflow:
  agents:
    a:
      extends: missing
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "is not a named agent");
    }

    #[test]
    fn agent_extends_cycle_errors() {
        let yaml = r#"
workflow:
  agents:
    a:
      extends: b
    b:
      extends: a
functions:
  f:
    input: { type: object }
    blocks:
      c:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "cyclic `extends`");
    }

    #[test]
    fn agent_write_paths_without_write_grant_warns() {
        let yaml = r#"
workflow:
  agents:
    a:
      grant: [tools]
      write_paths: [src/]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_clean(&r);
        assert!(r.warnings.iter().any(|w| w.message.contains("write_paths")));
    }

    #[test]
    fn agent_ref_unknown_named_errors() {
        let yaml = r#"
workflow:
  agents:
    a:
      model: sonnet
  agent: "$ref:#nope"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "is not a named agent");
    }

    // ---- Multiple errors collected in one pass ----

    #[test]
    fn collects_multiple_errors() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema:
          type: array
        transitions:
          - goto: nowhere
"#;
        let r = ok(yaml);
        assert!(
            r.errors.len() >= 2,
            "expected multiple errors, got {:#?}",
            r.errors
        );
    }

    // ---- Unreachable block warning ----

    #[test]
    fn depends_on_chain_is_reachable() {
        // `a` has no inbound -> entry; `orphan` has inbound (depends_on a) -> reachable.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
      orphan:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        depends_on: [a]
"#;
        assert_clean(&ok(yaml));
    }

    #[test]
    fn unreachable_block_warns() {
        // `orphan` is targeted only by itself (self-loop), so inbound is 1, not entry,
        // and not reached from any zero-inbound entry → warning.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
      orphan:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        transitions:
          - goto: orphan
"#;
        let r = ok(yaml);
        assert!(
            r.warnings.iter().any(|w| w.message.contains("unreachable")),
            "expected unreachable warning, got: {:#?}",
            r.warnings
        );
    }

    // ---- §12 worked example ----

    const FULL_EXAMPLE: &str = include_str!("schema/full_example.yaml");

    #[test]
    fn worked_example_validates_clean() {
        let wf = parse_workflow(FULL_EXAMPLE).expect("worked example parses");
        let known = KnownModels::new(["sonnet", "opus", "haiku"]);
        let r = validate_workflow(&wf, Some(Path::new("full_example.yaml")), &known);
        assert!(
            r.is_ok(),
            "worked example should validate clean, got errors: {:#?}",
            r.errors
        );
    }

    // ---- Deliverable 5: source location population ----

    #[test]
    fn issue_location_populated_for_block_field_error() {
        // `goto` to a missing transition target inside function `my_fn`, block `b1`.
        let yaml = r#"
functions:
  my_fn:
    input: { type: object }
    blocks:
      b1:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - goto: nowhere
"#;
        let wf = parse_workflow(yaml).expect("yaml parses");
        let path = Path::new("workflows/test.yaml");
        let r = validate_workflow(&wf, Some(path), &AnyModel);
        let issue = r
            .errors
            .iter()
            .find(|e| e.message.contains("transition target"))
            .unwrap_or_else(|| panic!("expected transition-target error; got {:#?}", r.errors));
        assert_eq!(issue.location.file.as_deref(), Some(path));
        assert_eq!(issue.location.function.as_deref(), Some("my_fn"));
        assert_eq!(issue.location.block.as_deref(), Some("b1"));
        assert_eq!(
            issue.location.field.as_deref(),
            Some("transitions[0].goto"),
            "expected field to be populated for field-level error"
        );
    }

    #[test]
    fn issue_location_populated_for_function_level_error() {
        // Invalid function name: block is None, function is Some, field is "name".
        let yaml = r#"
functions:
  BadFn:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let wf = parse_workflow(yaml).expect("yaml parses");
        let path = Path::new("workflows/func_err.yaml");
        let r = validate_workflow(&wf, Some(path), &AnyModel);
        let issue = r
            .errors
            .iter()
            .find(|e| e.message.contains("is not a valid identifier"))
            .unwrap_or_else(|| panic!("expected invalid-function-name error; got {:#?}", r.errors));
        assert_eq!(issue.location.file.as_deref(), Some(path));
        assert_eq!(issue.location.function.as_deref(), Some("BadFn"));
        assert_eq!(
            issue.location.block, None,
            "function-level error should have block == None"
        );
        assert_eq!(issue.location.field.as_deref(), Some("name"));
    }

    // ---- Deliverable 5 §10.1 coverage tests ----
    //
    // Several §10.1 checks are *structurally* unreachable from the YAML
    // grammar as modelled by our serde types. Where that's the case, the
    // tests below assert the parse-time error and explain the reasoning
    // loudly; they will fail (not silently skip) if parse behavior ever
    // changes to accept the malformed input.

    #[test]
    fn block_with_both_prompt_and_call_rejected() {
        // BlockDef is an untagged enum of PromptBlock | CallBlock, each
        // with `deny_unknown_fields`. A block containing both `prompt:`
        // and `call:` matches neither variant and is rejected at parse.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        call: other
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        input: { x: "1" }
"#;
        let err = parse_workflow(yaml).err().unwrap_or_else(|| {
            panic!(
                "expected a parse error for a block with both `prompt` and `call`; \
                 parser accepted the input — validator-level `exactly one` check must be added"
            )
        });
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("unknown")
                || msg.contains("did not match")
                || msg.contains("call")
                || msg.contains("prompt"),
            "parse error should indicate the variant mismatch, got: {msg}"
        );
    }

    #[test]
    fn block_with_neither_prompt_nor_call_rejected() {
        // Both PromptBlock and CallBlock have required fields (`prompt`+`schema`
        // and `call` respectively). A block with neither matches no variant
        // and parse fails.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        depends_on: []
"#;
        let err = parse_workflow(yaml).err().unwrap_or_else(|| {
            panic!(
                "expected a parse error for a block with neither `prompt` nor `call`; \
                 parser accepted the input — validator-level discrimination check must be added"
            )
        });
        let _ = err.to_string();
    }

    // Note: §10.1 "block name uniqueness within a function" and
    // "function name uniqueness" are structurally unreachable. Both are
    // modelled as `BTreeMap<String, _>` in `WorkflowFile`/`FunctionDef`,
    // which cannot hold duplicate keys at the struct level, and `serde_yml`
    // collapses duplicate YAML map keys to the last value. There is no
    // way to construct a `WorkflowFile` that violates these checks, so no
    // test is added — the §10.1 rows are satisfied by the type system.

    #[test]
    fn agent_grant_write_without_write_paths_clean() {
        // Positive: `grant: [write]` (with no tools set, no write_paths)
        // should not fire the `write_paths` normalization warning.
        let yaml = r#"
workflow:
  agents:
    a:
      grant: [write]
functions:
  f:
    input: { type: object }
    agent: "$ref:#a"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_clean(&r);
        assert!(
            !r.warnings.iter().any(|w| w.message.contains("write_paths")),
            "write_paths warning should NOT fire when `write` grant is present and write_paths is empty; got warnings: {:#?}",
            r.warnings
        );
    }

    #[test]
    fn agent_empty_grant_and_write_paths_pass_validation() {
        // `grant: []` should not produce "invalid grant" errors (zero iterations).
        // `write_paths: []` should not trigger the "write_paths is set but write
        // grant is not present" warning (empty vec → is_empty() == true).
        let yaml = r#"
workflow:
  agents:
    a:
      grant: []
      write_paths: []
functions:
  f:
    input: { type: object }
    agent: "$ref:#a"
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
"#;
        let r = ok(yaml);
        assert_clean(&r);
        assert!(
            !r.errors.iter().any(|e| e.message.contains("invalid grant")),
            "empty grant list should not produce invalid-grant errors; got: {:#?}",
            r.errors
        );
        assert!(
            !r.warnings.iter().any(|w| w.message.contains("write_paths")),
            "empty write_paths should not trigger write_paths warning; got: {:#?}",
            r.warnings
        );
    }

    #[test]
    fn per_call_entry_missing_fn_rejected_at_parse() {
        // CallEntry requires `fn` (renamed from `func`) with no serde default.
        // An entry omitting `fn` is rejected at parse time before validate runs.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        call:
          - input: { x: "1" }
"#;
        let err = parse_workflow(yaml).err().unwrap_or_else(|| {
            panic!(
                "expected parse error for per-call entry missing `fn`; parser accepted it — \
                 validator must grow an explicit check"
            )
        });
        let _ = err.to_string();
    }

    // Note: §10.1 "per-call entry missing `input`" is structurally unreachable.
    // `CallEntry.input` is `BTreeMap<String, Expr>` with no `#[serde(default)]`
    // and is required at parse time. An entry omitting `input` fails to parse
    // as a `CallEntry` and the untagged `CallSpec` enum then tries the
    // `Uniform(Vec<String>)` variant, which also fails for object entries.
    // There is no YAML input that reaches the validator with a per-call entry
    // whose `input` is missing; the check is satisfied by the parse layer.

    #[test]
    fn uniform_list_call_missing_block_input_errors() {
        let yaml = r#"
functions:
  a:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  b:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
  caller:
    input: { type: object }
    blocks:
      fanout:
        call: [a, b]
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "requires a block-level `input`");
    }

    #[test]
    fn parallel_siblings_conflicting_set_context_warns() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      shared: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        set_context:
          shared: "1"
      b:
        prompt: "hi"
        schema: { type: object, required: [k], properties: { k: { type: string } } }
        set_context:
          shared: "2"
"#;
        let r = ok(yaml);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.message.contains("may run in parallel") && w.message.contains("shared")),
            "expected parallel-write warning, got warnings: {:#?}",
            r.warnings
        );
    }

    // ---- CEL optional field safety tests -----------------------------------

    #[test]
    fn optional_field_safety_required_field_clean() {
        // Access to required field should not error.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
        set_context:
          n: "output.category"
"#;
        let r = ok(yaml);
        assert!(
            !r.errors
                .iter()
                .any(|e| e.message.contains("optional field safety")),
            "required field access should not trigger optional field safety; errors: {:#?}",
            r.errors
        );
    }

    #[test]
    fn optional_field_safety_optional_without_has_errors() {
        // Access to optional field without has() should error.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        set_context:
          n: "output.x"
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "optional field safety");
        assert_err_contains(&r, "output.x");
    }

    #[test]
    fn optional_field_safety_has_guard_clean() {
        // Access to optional field with has() guard should be clean.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      n: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        set_context:
          n: "has(output.x) && output.x > 0"
"#;
        let r = ok(yaml);
        assert!(
            !r.errors
                .iter()
                .any(|e| e.message.contains("optional field safety")),
            "has()-guarded access should not trigger optional field safety; errors: {:#?}",
            r.errors
        );
    }

    #[test]
    fn optional_field_safety_direct_has_clean() {
        // Expression using has(output.x) directly in the same expression.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        transitions:
          - when: "has(output.x) && output.x > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert!(
            !r.errors
                .iter()
                .any(|e| e.message.contains("optional field safety")),
            "has()-guarded access should not error; errors: {:#?}",
            r.errors
        );
    }

    #[test]
    fn optional_field_safety_input_namespace_errors() {
        // Access to optional field in input namespace should error.
        let yaml = r#"
functions:
  f:
    input:
      type: object
      required: [name]
      properties:
        name: { type: string }
        optional_field: { type: string }
    context:
      v: { type: string, initial: "" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        set_context:
          v: "input.optional_field"
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "optional field safety");
        assert_err_contains(&r, "input.optional_field");
    }

    #[test]
    fn optional_field_safety_blocks_namespace_errors() {
        // Access to optional field via blocks.NAME.output.FIELD should error.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      v: { type: integer, initial: 0 }
    blocks:
      prev:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            opt_field: { type: integer }
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        depends_on: [prev]
        set_context:
          v: "blocks.prev.output.opt_field"
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "optional field safety");
        assert_err_contains(&r, "opt_field");
    }

    #[test]
    fn optional_field_safety_nested_has_protection() {
        // has(output.x) protects output.x.y via prefix match.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      v: { type: string, initial: "" }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: object }
        set_context:
          v: "has(output.x) && output.x.y"
"#;
        let r = ok(yaml);
        assert!(
            !r.errors
                .iter()
                .any(|e| e.message.contains("optional field safety")),
            "prefix has() should protect deeper access; errors: {:#?}",
            r.errors
        );
    }

    #[test]
    fn optional_field_safety_context_workflow_no_check() {
        // context and workflow namespaces should not trigger the check.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      count: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
        transitions:
          - when: "context.count > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert!(
            !r.errors
                .iter()
                .any(|e| e.message.contains("optional field safety")),
            "context/workflow namespaces should not trigger safety check; errors: {:#?}",
            r.errors
        );
    }

    #[test]
    fn optional_field_safety_mixed_protected_and_unprotected() {
        // has(output.a) protects a; output.b is unprotected -> error only for b.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      v: { type: integer, initial: 0 }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            a: { type: integer }
            b: { type: integer }
        set_context:
          v: "has(output.a) && output.a > 0 && output.b > 0"
"#;
        let r = ok(yaml);
        let safety_errors: Vec<_> = r
            .errors
            .iter()
            .filter(|e| e.message.contains("optional field safety"))
            .collect();
        assert_eq!(
            safety_errors.len(),
            1,
            "expected exactly 1 optional field safety error, got: {:#?}",
            safety_errors
        );
        assert!(
            safety_errors[0].message.contains("output.b"),
            "error should be about output.b, got: {}",
            safety_errors[0].message
        );
    }

    #[test]
    fn optional_field_safety_when_guard_optional_errors() {
        // when guard accessing optional field without has() should error.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
            x: { type: integer }
        transitions:
          - when: "output.x > 0"
            goto: done
          - goto: done
      done:
        prompt: "hi"
        schema:
          type: object
          required: [k]
          properties: { k: { type: string } }
"#;
        let r = ok(yaml);
        assert_err_contains(&r, "optional field safety");
        assert_err_contains(&r, "output.x");
    }
}
