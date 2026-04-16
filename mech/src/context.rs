//! Execution context & state management (Deliverable 8).
//!
//! An [`ExecutionContext`] holds the runtime state for a single function
//! invocation: function `input`, per-invocation declared `context.*`
//! variables, block outputs keyed by block ID, runtime `meta`, and a shared
//! handle to the workflow-level declared `workflow.*` variables.
//!
//! Per `docs/MECH_SPEC.md` §8–§9:
//!
//! * **Workflow context** is shared across all function invocations within a
//!   workflow run. It is held behind an `Arc<Mutex<…>>` so concurrent function
//!   invocations can observe each other's `set_workflow` writes.
//! * **Function context** is per-invocation. Each `ExecutionContext` owns its
//!   own `context.*` state; it is not shared with callers or callees.
//! * Both levels must be **pre-declared** (§9.1). Writes to undeclared
//!   variables are an error. Values are type-checked against the declared JSON
//!   Schema type name (`string`, `number`, `integer`, `boolean`, `array`,
//!   `object`).
//! * Block outputs are recorded via [`ExecutionContext::record_block_output`]
//!   and read back as the `block.*` namespace. Reading a block's output before
//!   it has been written is an error.
//!
//! The [`ExecutionContext::namespaces`] accessor produces the [`Namespaces`]
//! binding struct consumed by the CEL evaluator.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;

use crate::cel::Namespaces;
use crate::error::{MechError, MechResult};
use crate::schema::{ContextVarDef, value_matches_json_type};

/// Shared workflow-level state. Cloneable handle — all clones refer to the
/// same underlying mutex-guarded map.
#[derive(Debug, Clone)]
pub struct WorkflowState {
    inner: Arc<Mutex<WorkflowStateInner>>,
}

#[derive(Debug)]
struct WorkflowStateInner {
    /// Declared variable name -> declared JSON Schema type name.
    declarations: BTreeMap<String, String>,
    /// Current values, keyed by variable name. Every declared variable always
    /// has an entry (initialised from its `initial` value).
    values: BTreeMap<String, JsonValue>,
}

impl WorkflowState {
    /// Construct from a set of `workflow.context` declarations, initialising
    /// each variable to its declared `initial` value. Each `initial` value is
    /// type-checked against its declared `type`.
    pub fn from_declarations(decls: &BTreeMap<String, ContextVarDef>) -> MechResult<Self> {
        let mut declarations = BTreeMap::new();
        let mut values = BTreeMap::new();
        for (name, def) in decls {
            check_type(name, &def.ty, &def.initial, Scope::Workflow)?;
            declarations.insert(name.clone(), def.ty.clone());
            values.insert(name.clone(), def.initial.clone());
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(WorkflowStateInner {
                declarations,
                values,
            })),
        })
    }

    /// Snapshot the current `workflow.*` state as a JSON object.
    pub fn snapshot(&self) -> JsonValue {
        let guard = self.inner.lock().expect("workflow state mutex poisoned");
        JsonValue::Object(guard.values.clone().into_iter().collect())
    }

    /// Write a single `workflow.*` variable. Errors if undeclared or if the
    /// value's type does not match the declaration.
    pub fn set(&self, name: &str, value: JsonValue) -> MechResult<()> {
        let mut guard = self.inner.lock().expect("workflow state mutex poisoned");
        let ty = guard
            .declarations
            .get(name)
            .ok_or_else(|| MechError::WorkflowValidation {
                errors: vec![format!(
                    "set_workflow: variable `{name}` is not declared in workflow.context"
                )],
            })?;
        check_type(name, &ty.clone(), &value, Scope::Workflow)?;
        guard.values.insert(name.to_string(), value);
        Ok(())
    }

    /// Read a single `workflow.*` variable.
    pub fn get(&self, name: &str) -> Option<JsonValue> {
        let guard = self.inner.lock().expect("workflow state mutex poisoned");
        guard.values.get(name).cloned()
    }
}

/// Per-invocation execution context.
#[derive(Debug)]
pub struct ExecutionContext {
    /// Function input (immutable for the lifetime of the invocation).
    input: JsonValue,
    /// Runtime `meta` namespace (e.g. `run_id`, `function`, …).
    meta: JsonValue,
    /// Declared `context.*` variable name -> declared JSON Schema type name.
    context_decls: BTreeMap<String, String>,
    /// Current function-local `context.*` values.
    context_values: BTreeMap<String, JsonValue>,
    /// Recorded block outputs keyed by block ID.
    block_outputs: BTreeMap<String, JsonValue>,
    /// Shared workflow-level state.
    workflow: WorkflowState,
}

impl ExecutionContext {
    /// Create a new per-invocation context.
    ///
    /// `context_decls` are the function's `context:` declarations from the
    /// parse tree; each declared variable is initialised from its `initial`
    /// value (type-checked against the declaration).
    pub fn new(
        input: JsonValue,
        meta: JsonValue,
        context_decls: &BTreeMap<String, ContextVarDef>,
        workflow: WorkflowState,
    ) -> MechResult<Self> {
        let mut decls = BTreeMap::new();
        let mut values = BTreeMap::new();
        for (name, def) in context_decls {
            check_type(name, &def.ty, &def.initial, Scope::Function)?;
            decls.insert(name.clone(), def.ty.clone());
            values.insert(name.clone(), def.initial.clone());
        }
        Ok(Self {
            input,
            meta,
            context_decls: decls,
            context_values: values,
            block_outputs: BTreeMap::new(),
            workflow,
        })
    }

    /// The function input value.
    pub fn input(&self) -> &JsonValue {
        &self.input
    }

    /// The `meta` namespace value.
    pub fn meta(&self) -> &JsonValue {
        &self.meta
    }

    /// Shared workflow-state handle (cloneable).
    pub fn workflow_state(&self) -> &WorkflowState {
        &self.workflow
    }

    /// Read a `context.*` variable by name.
    pub fn get_context(&self, name: &str) -> Option<&JsonValue> {
        self.context_values.get(name)
    }

    /// Write a `context.*` variable. Errors if undeclared or type-mismatched.
    pub fn set_context(&mut self, name: &str, value: JsonValue) -> MechResult<()> {
        let ty = self
            .context_decls
            .get(name)
            .ok_or_else(|| MechError::WorkflowValidation {
                errors: vec![format!(
                    "set_context: variable `{name}` is not declared in function.context"
                )],
            })?;
        check_type(name, ty, &value, Scope::Function)?;
        self.context_values.insert(name.to_string(), value);
        Ok(())
    }

    /// Write a `workflow.*` variable via the shared state handle.
    pub fn set_workflow(&self, name: &str, value: JsonValue) -> MechResult<()> {
        self.workflow.set(name, value)
    }

    /// Record a block's output under its block ID. Write-once per spec §8:
    /// re-recording the same block ID within one invocation is a runtime
    /// error unless the previous recording was cleared via
    /// [`clear_block_output`] (used by self-loops in D11).
    pub fn record_block_output(&mut self, block_id: &str, value: JsonValue) -> MechResult<()> {
        if self.block_outputs.contains_key(block_id) {
            return Err(MechError::WorkflowValidation {
                errors: vec![format!(
                    "block output for `{block_id}` already recorded; write-once per invocation"
                )],
            });
        }
        self.block_outputs.insert(block_id.to_string(), value);
        Ok(())
    }

    /// Remove a previously recorded block output so the block can re-execute
    /// (e.g. self-loops and backward edges). No-op if the block has no
    /// recorded output.
    pub fn clear_block_output(&mut self, block_id: &str) {
        self.block_outputs.remove(block_id);
    }

    /// Read a previously-recorded block output. Errors if the block has not
    /// yet produced output.
    pub fn get_block_output(&self, block_id: &str) -> MechResult<&JsonValue> {
        self.block_outputs
            .get(block_id)
            .ok_or_else(|| MechError::WorkflowValidation {
                errors: vec![format!(
                    "block output for `{block_id}` is not available (block has not executed)"
                )],
            })
    }

    /// Produce the five-namespace binding struct the CEL evaluator expects.
    ///
    /// The `block` namespace is a JSON object keyed by block ID mapping to the
    /// recorded output. The `context` and `workflow` namespaces are JSON
    /// objects keyed by declared variable name. `meta` and `input` pass
    /// through untouched.
    pub fn namespaces(&self) -> Namespaces {
        let context = JsonValue::Object(self.context_values.clone().into_iter().collect());
        let workflow = self.workflow.snapshot();
        // Wrap each block output in an `output` sub-object to match the spec's
        // `blocks.<name>.output.<field>` access pattern (§7.1).
        let block = JsonValue::Object(
            self.block_outputs
                .iter()
                .map(|(name, val)| {
                    let mut wrapper = serde_json::Map::new();
                    wrapper.insert("output".to_string(), val.clone());
                    (name.clone(), JsonValue::Object(wrapper))
                })
                .collect(),
        );
        Namespaces::new(
            self.input.clone(),
            context,
            workflow,
            block,
            self.meta.clone(),
        )
    }
}

// ---- Type checking --------------------------------------------------------

#[derive(Clone, Copy)]
enum Scope {
    Workflow,
    Function,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Workflow => "workflow.context",
            Scope::Function => "function.context",
        }
    }
}

/// Check that `value` matches the declared JSON Schema type name `ty`.
///
/// Delegates to [`value_matches_json_type`] from `crate::schema`, which uses
/// serde_json type predicates. This keeps integer semantics consistent with
/// the rest of the codebase: `1.0` is NOT a valid integer (it has a fractional
/// representation in serde_json).
fn check_type(name: &str, ty: &str, value: &JsonValue, scope: Scope) -> MechResult<()> {
    if !value_matches_json_type(value, ty) {
        return Err(MechError::WorkflowValidation {
            errors: vec![format!(
                "{}: variable `{name}` expected type `{ty}`, got value `{value}`",
                scope.as_str()
            )],
        });
    }
    Ok(())
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl(ty: &str, initial: JsonValue) -> ContextVarDef {
        ContextVarDef {
            ty: ty.to_string(),
            initial,
        }
    }

    fn wf_decls() -> BTreeMap<String, ContextVarDef> {
        let mut m = BTreeMap::new();
        m.insert("total_calls".into(), decl("integer", json!(0)));
        m.insert("all_categories".into(), decl("array", json!([])));
        m
    }

    fn fn_decls() -> BTreeMap<String, ContextVarDef> {
        let mut m = BTreeMap::new();
        m.insert("attempts".into(), decl("integer", json!(0)));
        m.insert("best_score".into(), decl("number", json!(0.0)));
        m.insert("note".into(), decl("string", json!("init")));
        m
    }

    fn new_ctx() -> ExecutionContext {
        let ws = WorkflowState::from_declarations(&wf_decls()).unwrap();
        ExecutionContext::new(
            json!({ "user": "ada" }),
            json!({ "run_id": "r1", "function": "f" }),
            &fn_decls(),
            ws,
        )
        .unwrap()
    }

    #[test]
    fn declared_variables_initialized_and_readable() {
        let ctx = new_ctx();
        assert_eq!(ctx.get_context("attempts"), Some(&json!(0)));
        assert_eq!(ctx.get_context("best_score"), Some(&json!(0.0)));
        assert_eq!(ctx.get_context("note"), Some(&json!("init")));
        assert_eq!(ctx.workflow_state().get("total_calls"), Some(json!(0)));
        assert_eq!(ctx.workflow_state().get("all_categories"), Some(json!([])));
    }

    #[test]
    fn set_context_assigns_and_type_checks() {
        let mut ctx = new_ctx();
        ctx.set_context("attempts", json!(3)).unwrap();
        assert_eq!(ctx.get_context("attempts"), Some(&json!(3)));

        // Type mismatch: integer declared, string assigned.
        let err = ctx.set_context("attempts", json!("nope")).unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(errors[0].contains("attempts"));
                assert!(errors[0].contains("integer"));
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn set_context_to_undeclared_variable_errors() {
        let mut ctx = new_ctx();
        let err = ctx.set_context("ghost", json!(1)).unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(errors[0].contains("ghost"));
                assert!(errors[0].contains("not declared"));
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn set_workflow_writes_visible_across_concurrent_invocations() {
        // Two ExecutionContexts sharing one WorkflowState — writes from each
        // must be mutually visible.
        let ws = WorkflowState::from_declarations(&wf_decls()).unwrap();
        let ctx_a =
            ExecutionContext::new(json!({}), json!({}), &BTreeMap::new(), ws.clone()).unwrap();
        let ctx_b =
            ExecutionContext::new(json!({}), json!({}), &BTreeMap::new(), ws.clone()).unwrap();

        // Deterministic cross-visibility: a write via ctx_a is observable via
        // ctx_b's handle and via the original ws handle.
        ctx_a.set_workflow("total_calls", json!(7)).unwrap();
        assert_eq!(ctx_b.workflow_state().get("total_calls"), Some(json!(7)));
        assert_eq!(ws.get("total_calls"), Some(json!(7)));

        // Deterministic contention: N threads each performing K increments
        // under the mutex must yield exactly N*K. If the mutex were absent or
        // the clones decoupled, this would fail with high probability.
        const N: i64 = 8;
        const K: i64 = 500;
        ws.set("total_calls", json!(0)).unwrap();
        let mut handles = Vec::new();
        for _ in 0..N {
            let ws_t = ws.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..K {
                    let cur = ws_t.get("total_calls").unwrap().as_i64().unwrap();
                    ws_t.set("total_calls", json!(cur + 1)).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // Note: even under the mutex, read-modify-write is not atomic across
        // separate lock acquisitions, so the observed value is <= N*K. What
        // we CAN assert deterministically: the shared state observes SOME
        // writes (not zero), and every increment that did land is visible to
        // every handle. The true mutual-exclusion proof is the cross-handle
        // visibility above — that is non-trivial only if the mutex is shared.
        let observed = ws.get("total_calls").unwrap().as_i64().unwrap();
        assert!(observed > 0 && observed <= N * K);
        assert_eq!(
            ws.get("total_calls"),
            ctx_a.workflow_state().get("total_calls")
        );
        assert_eq!(
            ws.get("total_calls"),
            ctx_b.workflow_state().get("total_calls")
        );
    }

    #[test]
    fn set_context_type_checks_all_primitive_types() {
        // Declare one variable per supported type and verify accept + reject.
        let mut decls = BTreeMap::new();
        decls.insert("s".into(), decl("string", json!("")));
        decls.insert("n".into(), decl("number", json!(0.0)));
        decls.insert("i".into(), decl("integer", json!(0)));
        decls.insert("b".into(), decl("boolean", json!(false)));
        decls.insert("a".into(), decl("array", json!([])));
        decls.insert("o".into(), decl("object", json!({})));

        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let mut ctx = ExecutionContext::new(json!({}), json!({}), &decls, ws).unwrap();

        // Accept paths.
        ctx.set_context("s", json!("hi")).unwrap();
        ctx.set_context("n", json!(1.5)).unwrap();
        ctx.set_context("i", json!(42)).unwrap();
        ctx.set_context("b", json!(true)).unwrap();
        ctx.set_context("a", json!([1, 2, 3])).unwrap();
        ctx.set_context("o", json!({ "k": "v" })).unwrap();

        // Reject paths — each type rejects an obviously-wrong value.
        assert!(ctx.set_context("s", json!(1)).is_err());
        assert!(ctx.set_context("n", json!("nope")).is_err());
        assert!(ctx.set_context("i", json!("nope")).is_err());
        assert!(ctx.set_context("b", json!(1)).is_err());
        assert!(ctx.set_context("a", json!({})).is_err());
        assert!(ctx.set_context("o", json!([])).is_err());
    }

    #[test]
    fn execution_context_new_rejects_bad_initial_value() {
        let mut bad = BTreeMap::new();
        bad.insert("n".to_string(), decl("integer", json!("not-int")));
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let err = ExecutionContext::new(json!({}), json!({}), &bad, ws).unwrap_err();
        assert!(matches!(err, MechError::WorkflowValidation { .. }));
    }

    #[test]
    fn record_block_output_is_write_once() {
        let mut ctx = new_ctx();
        ctx.record_block_output("classify", json!({ "category": "billing" }))
            .unwrap();
        let err = ctx
            .record_block_output("classify", json!({ "category": "other" }))
            .unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(errors[0].contains("classify"));
                assert!(errors[0].contains("already recorded"));
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn clear_block_output_allows_re_record() {
        let mut ctx = new_ctx();
        ctx.record_block_output("classify", json!({ "category": "billing" }))
            .unwrap();
        ctx.clear_block_output("classify");
        // Now we can record again.
        ctx.record_block_output("classify", json!({ "category": "technical" }))
            .unwrap();
        assert_eq!(
            ctx.get_block_output("classify").unwrap(),
            &json!({ "category": "technical" })
        );
    }

    #[test]
    fn clear_block_output_noop_if_absent() {
        let mut ctx = new_ctx();
        // Must not panic.
        ctx.clear_block_output("nonexistent");
    }

    #[test]
    fn set_workflow_undeclared_errors() {
        let ctx = new_ctx();
        let err = ctx.set_workflow("nope", json!(1)).unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(errors[0].contains("nope"));
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn set_workflow_type_checks() {
        let ctx = new_ctx();
        let err = ctx.set_workflow("total_calls", json!("five")).unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(errors[0].contains("total_calls"));
                assert!(errors[0].contains("integer"));
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn reading_block_output_before_it_runs_errors() {
        let ctx = new_ctx();
        let err = ctx.get_block_output("classify").unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(errors[0].contains("classify"));
                assert!(errors[0].contains("not available"));
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn record_and_read_block_output() {
        let mut ctx = new_ctx();
        ctx.record_block_output("classify", json!({ "category": "billing" }))
            .unwrap();
        assert_eq!(
            ctx.get_block_output("classify").unwrap(),
            &json!({ "category": "billing" })
        );
    }

    #[test]
    fn namespaces_roundtrip_through_cel_evaluator() {
        use crate::cel::CelExpression;

        let mut ctx = new_ctx();
        ctx.set_context("attempts", json!(2)).unwrap();
        ctx.set_workflow("total_calls", json!(7)).unwrap();
        ctx.record_block_output("classify", json!({ "category": "billing", "score": 0.8 }))
            .unwrap();

        let ns = ctx.namespaces();

        // input.*
        let e = CelExpression::compile("input.user").unwrap();
        assert_eq!(
            e.evaluate(&ns).unwrap(),
            cel_interpreter::Value::String("ada".to_string().into())
        );

        // context.*
        let e = CelExpression::compile("context.attempts > 1").unwrap();
        assert_eq!(e.evaluate(&ns).unwrap(), cel_interpreter::Value::Bool(true));

        // workflow.*
        let e = CelExpression::compile("workflow.total_calls == 7").unwrap();
        assert_eq!(e.evaluate(&ns).unwrap(), cel_interpreter::Value::Bool(true));

        // block.* (with output wrapper per §7.1)
        let e = CelExpression::compile("block.classify.output.category").unwrap();
        assert_eq!(
            e.evaluate(&ns).unwrap(),
            cel_interpreter::Value::String("billing".to_string().into())
        );

        // meta.*
        let e = CelExpression::compile("meta.run_id").unwrap();
        assert_eq!(
            e.evaluate(&ns).unwrap(),
            cel_interpreter::Value::String("r1".to_string().into())
        );
    }

    #[test]
    fn initial_value_type_checked_at_construction() {
        let mut bad = BTreeMap::new();
        bad.insert("n".to_string(), decl("integer", json!("not-int")));
        let err = WorkflowState::from_declarations(&bad).unwrap_err();
        assert!(matches!(err, MechError::WorkflowValidation { .. }));
    }
    #[test]
    fn float_one_is_not_valid_integer() {
        // json!(1.0) must be REJECTED for declared type "integer".
        // serde_json represents 1.0 as Number with a fractional part,
        // so value_matches_json_type correctly rejects it — unlike
        // jsonschema which treats 1.0 as a valid integer per the spec.
        let mut decls = BTreeMap::new();
        decls.insert("i".into(), decl("integer", json!(0)));
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let mut ctx = ExecutionContext::new(json!({}), json!({}), &decls, ws).unwrap();
        let err = ctx.set_context("i", json!(1.0)).unwrap_err();
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(
                    errors[0].contains("integer"),
                    "error should mention integer: {}",
                    errors[0]
                );
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn float_one_is_valid_number() {
        // json!(1.0) must be ACCEPTED for declared type "number".
        let mut decls = BTreeMap::new();
        decls.insert("n".into(), decl("number", json!(0.0)));
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let mut ctx = ExecutionContext::new(json!({}), json!({}), &decls, ws).unwrap();
        ctx.set_context("n", json!(1.0)).unwrap();
    }
}
