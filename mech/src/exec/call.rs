//! Call block executor.
//!
//! Executes a single [`CallBlock`]:
//!
//! 1. Resolve the list of function calls from [`CallSpec`] (single, uniform
//!    list, or per-call list).
//! 2. Render input mapping templates against the current
//!    [`ExecutionContext::namespaces`].
//! 3. Invoke each function sequentially via the injected
//!    [`FunctionExecutor`].
//! 4. Compute the block output: if an explicit `output` mapping is declared,
//!    evaluate its CEL expressions against augmented namespaces that include
//!    each function result as `<fn_name>.output.*`; otherwise apply the
//!    default (single → function return, list → last function return).
//! 5. Record the block output via
//!    [`ExecutionContext::record_block_output`].
//!
//! Transitions, `set_context` / `set_workflow` side-effects, and block
//! scheduling live in [`crate::exec::schedule`].

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::cel::Namespaces;
use crate::context::ExecutionContext;
use crate::error::{MechError, MechResult};
use crate::exec::agent::BoxFuture;
use crate::schema::{CallBlock, CallSpec, FunctionDef};
use crate::workflow::Workflow;

/// Callback for invoking a function by name. The workflow driver supplies
/// the production implementation; tests inject a fake.
pub trait FunctionExecutor: Send + Sync {
    /// Invoke a named function with the given resolved input.
    fn call<'a>(
        &'a self,
        function_name: &'a str,
        input: JsonValue,
    ) -> BoxFuture<'a, Result<JsonValue, MechError>>;
}

/// A single resolved function call: name + rendered input.
struct ResolvedCall {
    fn_name: String,
    input: JsonValue,
}

/// Render a mapping of `field_name → template_expr` into a JSON object using
/// the workflow's interned templates. Each template is evaluated as a JSON
/// value (preserving CEL types for pure `{{expr}}` templates). Used for both
/// input and output mappings on call blocks.
fn render_mapping(
    workflow: &Workflow,
    mapping: &BTreeMap<String, String>,
    namespaces: &Namespaces,
) -> MechResult<JsonValue> {
    let mut obj = serde_json::Map::with_capacity(mapping.len());
    for (key, expr) in mapping {
        let tmpl = workflow
            .template(expr)
            .ok_or_else(|| MechError::InternalInvariant {
                message: format!("template `{expr}` should have been interned at load time"),
            })?;
        let value = tmpl.evaluate_as_json(namespaces)?;
        obj.insert(key.clone(), value);
    }
    Ok(JsonValue::Object(obj))
}

/// Resolve the list of function calls from a [`CallSpec`] and the block's
/// input mapping, rendering each input against the current namespaces.
fn resolve_calls(
    workflow: &Workflow,
    block: &CallBlock,
    namespaces: &Namespaces,
) -> MechResult<Vec<ResolvedCall>> {
    match &block.call {
        CallSpec::Single(name) => {
            let input_mapping =
                block
                    .input
                    .as_ref()
                    .ok_or_else(|| MechError::WorkflowValidation {
                        errors: vec![format!(
                            "call block: single function call `{name}` requires block-level `input`"
                        )],
                    })?;
            let input = render_mapping(workflow, input_mapping, namespaces)?;
            Ok(vec![ResolvedCall {
                fn_name: name.clone(),
                input,
            }])
        }
        CallSpec::Uniform(names) => {
            let input_mapping =
                block
                    .input
                    .as_ref()
                    .ok_or_else(|| MechError::WorkflowValidation {
                        errors: vec![
                            "call block: uniform list call requires block-level `input`".into(),
                        ],
                    })?;
            let input = render_mapping(workflow, input_mapping, namespaces)?;
            Ok(names
                .iter()
                .map(|name| ResolvedCall {
                    fn_name: name.clone(),
                    input: input.clone(),
                })
                .collect())
        }
        CallSpec::PerCall(entries) => {
            let mut calls = Vec::with_capacity(entries.len());
            for entry in entries {
                let input = render_mapping(workflow, &entry.input, namespaces)?;
                calls.push(ResolvedCall {
                    fn_name: entry.func.clone(),
                    input,
                });
            }
            Ok(calls)
        }
    }
}

/// Check that every called function exists in the workflow. Defense in depth —
/// the loader should have caught this, but runtime guards are cheap.
pub(crate) fn validate_function_exists(
    workflow: &Workflow,
    fn_name: &str,
    block_id: &str,
) -> MechResult<()> {
    if !workflow.document().functions.contains_key(fn_name) {
        return Err(MechError::WorkflowValidation {
            errors: vec![format!(
                "call block `{block_id}`: function `{fn_name}` is not declared in the workflow"
            )],
        });
    }
    Ok(())
}

/// Build augmented namespaces for output mapping evaluation. Each function
/// result is available as a top-level CEL variable `<fn_name>` with an
/// `output` subfield: `<fn_name>.output.*`.
fn build_output_mapping_namespaces(
    ctx: &ExecutionContext,
    results: &[(String, JsonValue)],
) -> Namespaces {
    let base = ctx.namespaces();
    let mut extras = BTreeMap::new();
    for (fn_name, output) in results {
        let mut wrapper = serde_json::Map::new();
        wrapper.insert("output".to_string(), output.clone());
        extras.insert(fn_name.clone(), JsonValue::Object(wrapper));
    }
    Namespaces::with_extras(
        base.input,
        base.context,
        base.workflow,
        base.blocks,
        base.meta,
        extras,
    )
}

/// Execute a single call block.
///
/// Resolves input, invokes functions sequentially, computes block output,
/// and records it in the execution context. Transitions and `set_context` /
/// `set_workflow` writes are handled by [`crate::exec::schedule`].
pub async fn execute_call_block(
    workflow: &Workflow,
    _function: &FunctionDef,
    block_id: &str,
    block: &CallBlock,
    ctx: &mut ExecutionContext,
    func_executor: &dyn FunctionExecutor,
) -> MechResult<JsonValue> {
    // 1. Resolve the calls: function names + rendered inputs.
    let namespaces = ctx.namespaces();
    let calls = resolve_calls(workflow, block, &namespaces)?;

    // 2. Validate all called functions exist (defense in depth).
    for call in &calls {
        validate_function_exists(workflow, &call.fn_name, block_id)?;
    }

    // 3. Execute functions sequentially, collecting results.
    let mut results: Vec<(String, JsonValue)> = Vec::with_capacity(calls.len());
    for call in calls {
        let output = func_executor.call(&call.fn_name, call.input).await?;
        results.push((call.fn_name, output));
    }

    // 4. Compute the block output.
    let output = if let Some(output_mapping) = &block.output {
        // Explicit output mapping: evaluate CEL expressions against augmented
        // namespaces that include function results.
        let mapping_ns = build_output_mapping_namespaces(ctx, &results);
        render_mapping(workflow, output_mapping, &mapping_ns)?
    } else {
        // Default: for single function, return its output; for lists, return
        // the last function's output.
        results
            .last()
            .map(|(_, v)| v.clone())
            .unwrap_or(JsonValue::Null)
    };

    // 5. Record the block output.
    ctx.record_block_output(block_id, output.clone())?;
    Ok(output)
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, WorkflowState};
    use crate::exec::agent::BoxFuture;
    use crate::loader::WorkflowLoader;
    use crate::schema::BlockDef;
    use serde_json::json;
    use std::sync::Mutex;

    /// Fake function executor that captures calls and returns canned responses.
    struct FakeFuncExecutor {
        /// Canned responses keyed by function name.
        responses: BTreeMap<String, JsonValue>,
        /// Captured calls in order: (fn_name, input).
        calls: Mutex<Vec<(String, JsonValue)>>,
    }

    impl FakeFuncExecutor {
        fn new(responses: BTreeMap<String, JsonValue>) -> Self {
            Self {
                responses,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn captured_calls(&self) -> Vec<(String, JsonValue)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl FunctionExecutor for FakeFuncExecutor {
        fn call<'a>(
            &'a self,
            function_name: &'a str,
            input: JsonValue,
        ) -> BoxFuture<'a, Result<JsonValue, MechError>> {
            self.calls
                .lock()
                .unwrap()
                .push((function_name.to_string(), input));
            let result = self.responses.get(function_name).cloned().ok_or_else(|| {
                MechError::WorkflowValidation {
                    errors: vec![format!("fake: no canned response for `{function_name}`")],
                }
            });
            Box::pin(async move { result })
        }
    }

    fn run_blocking<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(fut)
    }

    fn load(yaml: &str) -> crate::Workflow {
        WorkflowLoader::new().load_str(yaml).expect("load")
    }

    fn new_ctx(
        input: JsonValue,
        fn_decls: &BTreeMap<String, crate::schema::ContextVarDef>,
    ) -> ExecutionContext {
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        ExecutionContext::new(input, json!({ "run_id": "r1" }), fn_decls, ws).unwrap()
    }

    fn get_call_block(wf: &crate::Workflow, fn_name: &str, block_name: &str) -> CallBlock {
        let func = wf.document().functions.get(fn_name).unwrap();
        match &func.blocks[block_name] {
            BlockDef::Call(c) => c.clone(),
            _ => panic!("expected call block"),
        }
    }

    // ---- T1: Single call with shared input --------------------------------

    const SINGLE_CALL: &str = r#"
functions:
  caller:
    input: { type: object }
    blocks:
      do_call:
        call: callee
        input:
          text: "{{input.user_text}}"
          count: "{{input.n}}"
  callee:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn single_call_with_shared_input() {
        let wf = load(SINGLE_CALL);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "do_call");
        let mut ctx = new_ctx(json!({ "user_text": "hello", "n": 42 }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("callee".into(), json!({ "result": "ok" }));
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "do_call", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        // Output is the function's return (no output mapping).
        assert_eq!(out, json!({ "result": "ok" }));

        // Block output recorded in context.
        assert_eq!(
            ctx.get_block_output("do_call").unwrap(),
            &json!({ "result": "ok" })
        );

        // Input was rendered correctly.
        let calls = executor.captured_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "callee");
        assert_eq!(calls[0].1, json!({ "text": "hello", "count": 42 }));
    }

    // ---- T2: Uniform list all receive same input --------------------------

    const UNIFORM_LIST: &str = r#"
functions:
  caller:
    input: { type: object }
    output:
      type: object
    blocks:
      pipeline:
        call: [step_a, step_b, step_c]
        input:
          text: "{{input.text}}"
  step_a:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
  step_b:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
  step_c:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn uniform_list_all_receive_same_input() {
        let wf = load(UNIFORM_LIST);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "pipeline");
        let mut ctx = new_ctx(json!({ "text": "shared" }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("step_a".into(), json!({ "a": 1 }));
        responses.insert("step_b".into(), json!({ "b": 2 }));
        responses.insert("step_c".into(), json!({ "c": 3 }));
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "pipeline", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        // Default output for list: last function's return.
        assert_eq!(out, json!({ "c": 3 }));

        // All three called with same input.
        let calls = executor.captured_calls();
        assert_eq!(calls.len(), 3);
        for (name, input) in &calls {
            assert_eq!(input, &json!({ "text": "shared" }), "fn {name}");
        }
        assert_eq!(calls[0].0, "step_a");
        assert_eq!(calls[1].0, "step_b");
        assert_eq!(calls[2].0, "step_c");
    }

    // ---- T3: Per-call list heterogeneous input ----------------------------

    const PER_CALL: &str = r#"
functions:
  caller:
    input: { type: object }
    output:
      type: object
    blocks:
      analyze:
        call:
          - fn: sentiment
            input:
              text: "{{input.text}}"
          - fn: classify
            input:
              category: "{{input.cat}}"
              lang: "{{input.lang}}"
  sentiment:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
  classify:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn per_call_list_heterogeneous_input() {
        let wf = load(PER_CALL);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "analyze");
        let mut ctx = new_ctx(
            json!({ "text": "great product", "cat": "review", "lang": "en" }),
            &BTreeMap::new(),
        );

        let mut responses = BTreeMap::new();
        responses.insert("sentiment".into(), json!({ "score": 0.9 }));
        responses.insert("classify".into(), json!({ "label": "positive" }));
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "analyze", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        // Default for list: last function's return.
        assert_eq!(out, json!({ "label": "positive" }));

        let calls = executor.captured_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "sentiment");
        assert_eq!(calls[0].1, json!({ "text": "great product" }));
        assert_eq!(calls[1].0, "classify");
        assert_eq!(calls[1].1, json!({ "category": "review", "lang": "en" }));
    }

    // ---- T4: Output mapping constructs block output -----------------------

    const WITH_OUTPUT_MAPPING: &str = r#"
functions:
  caller:
    input: { type: object }
    output:
      type: object
      required: [mood, rules]
      properties:
        mood: { type: number }
        rules: { type: array }
    blocks:
      analyze:
        call:
          - fn: sentiment
            input:
              text: "{{input.text}}"
          - fn: policy
            input:
              query: "{{input.text}}"
        output:
          mood: "{{sentiment.output.score}}"
          rules: "{{policy.output.policies}}"
  sentiment:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
  policy:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn output_mapping_constructs_block_output() {
        let wf = load(WITH_OUTPUT_MAPPING);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "analyze");
        let mut ctx = new_ctx(json!({ "text": "billing issue" }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("sentiment".into(), json!({ "score": 0.3 }));
        responses.insert(
            "policy".into(),
            json!({ "policies": ["refund_policy", "escalation_policy"] }),
        );
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "analyze", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        assert_eq!(
            out,
            json!({
                "mood": 0.3,
                "rules": ["refund_policy", "escalation_policy"]
            })
        );
        assert_eq!(ctx.get_block_output("analyze").unwrap(), &out);
    }

    // ---- T5: Undeclared function errors -----------------------------------

    const UNDECLARED_FN: &str = r#"
functions:
  caller:
    input: { type: object }
    blocks:
      do_call:
        call: nonexistent
        input:
          x: "{{input.x}}"
"#;

    #[test]
    fn undeclared_function_errors() {
        // The loader's validation normally rejects this. We force a load that
        // skips that check by constructing the Workflow manually. Instead,
        // since the loader DOES reject it, we test the defense-in-depth by
        // calling execute_call_block directly with a workflow where the
        // function is present at parse-time but we'll test via a modified
        // scenario.
        //
        // Actually, the loader rejects references to undeclared functions.
        // To test the runtime check, we need a workflow where the call
        // references an existing function. Let's verify the loader rejects
        // the undeclared case.
        let result = WorkflowLoader::new().load_str(UNDECLARED_FN);
        assert!(result.is_err(), "loader must reject undeclared function");
        match result.unwrap_err() {
            MechError::WorkflowValidation { errors } => {
                assert!(
                    errors.iter().any(|e| e.contains("nonexistent")),
                    "error should mention the undeclared function: {errors:?}"
                );
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    // ---- T6: Default output for single function ---------------------------

    #[test]
    fn default_output_single_fn_returns_function_output() {
        // Reuse SINGLE_CALL fixture — no output mapping, single fn.
        let wf = load(SINGLE_CALL);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "do_call");
        let mut ctx = new_ctx(json!({ "user_text": "hi", "n": 1 }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("callee".into(), json!({ "answer": 42, "ok": true }));
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "do_call", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        assert_eq!(out, json!({ "answer": 42, "ok": true }));
    }

    // ---- T7: Default output for list is last function's return ------------

    #[test]
    fn default_output_list_returns_last_function_output() {
        // Reuse UNIFORM_LIST fixture.
        let wf = load(UNIFORM_LIST);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "pipeline");
        let mut ctx = new_ctx(json!({ "text": "test" }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("step_a".into(), json!({ "first": true }));
        responses.insert("step_b".into(), json!({ "second": true }));
        responses.insert("step_c".into(), json!({ "last": true }));
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "pipeline", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        assert_eq!(out, json!({ "last": true }));
    }

    // ---- T8: Input template reads all namespaces --------------------------

    const NAMESPACE_INPUT: &str = r#"
workflow:
  context:
    global_val: { type: integer, initial: 99 }
functions:
  caller:
    input: { type: object }
    context:
      local_note: { type: string, initial: "ctx_hello" }
    blocks:
      do_call:
        call: callee
        input:
          from_input: "{{input.user}}"
          from_context: "{{context.local_note}}"
          from_workflow: "{{workflow.global_val}}"
  callee:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn input_template_reads_all_namespaces() {
        let wf = load(NAMESPACE_INPUT);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "do_call");
        let fn_decls = func.context.clone();
        let ws = WorkflowState::from_declarations(
            &wf.document()
                .workflow
                .as_ref()
                .map(|w| w.context.clone())
                .unwrap_or_default(),
        )
        .unwrap();
        let mut ctx = ExecutionContext::new(
            json!({ "user": "ada" }),
            json!({ "run_id": "r1" }),
            &fn_decls,
            ws,
        )
        .unwrap();

        let mut responses = BTreeMap::new();
        responses.insert("callee".into(), json!({ "ok": true }));
        let executor = FakeFuncExecutor::new(responses);

        run_blocking(execute_call_block(
            &wf, func, "do_call", &block, &mut ctx, &executor,
        ))
        .unwrap();

        let calls = executor.captured_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["from_input"], json!("ada"));
        assert_eq!(calls[0].1["from_context"], json!("ctx_hello"));
        assert_eq!(calls[0].1["from_workflow"], json!(99));
    }

    // ---- T9: Input reads block namespace for prior block outputs ----------

    const BLOCK_NS_INPUT: &str = r#"
functions:
  caller:
    input: { type: object }
    output:
      type: object
    blocks:
      prior:
        prompt: "stub"
        schema:
          type: object
          required: [value]
          properties:
            value: { type: string }
      do_call:
        call: callee
        input:
          prev_result: "{{block.prior.output.value}}"
        depends_on: [prior]
  callee:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn input_template_reads_block_namespace() {
        let wf = load(BLOCK_NS_INPUT);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "do_call");
        let mut ctx = new_ctx(json!({}), &BTreeMap::new());

        // Simulate the prior block having executed.
        ctx.record_block_output("prior", json!({ "value": "from_prior" }))
            .unwrap();

        let mut responses = BTreeMap::new();
        responses.insert("callee".into(), json!({ "done": true }));
        let executor = FakeFuncExecutor::new(responses);

        run_blocking(execute_call_block(
            &wf, func, "do_call", &block, &mut ctx, &executor,
        ))
        .unwrap();

        let calls = executor.captured_calls();
        assert_eq!(calls[0].1["prev_result"], json!("from_prior"));
    }

    // ---- T10: Sequential execution order preserved ------------------------

    #[test]
    fn sequential_execution_order_preserved() {
        let wf = load(UNIFORM_LIST);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "pipeline");
        let mut ctx = new_ctx(json!({ "text": "order_test" }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("step_a".into(), json!(1));
        responses.insert("step_b".into(), json!(2));
        responses.insert("step_c".into(), json!(3));
        let executor = FakeFuncExecutor::new(responses);

        run_blocking(execute_call_block(
            &wf, func, "pipeline", &block, &mut ctx, &executor,
        ))
        .unwrap();

        let calls = executor.captured_calls();
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["step_a", "step_b", "step_c"]);
    }

    // ---- T11: Output mapping with single call fn --------------------------

    const SINGLE_WITH_OUTPUT: &str = r#"
functions:
  caller:
    input: { type: object }
    output:
      type: object
      required: [summary, flag]
      properties:
        summary: { type: string }
        flag: { type: boolean }
    blocks:
      do_call:
        call: callee
        input:
          text: "{{input.text}}"
        output:
          summary: "{{callee.output.result}}"
          flag: "{{callee.output.ok}}"
  callee:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;

    #[test]
    fn output_mapping_with_single_call() {
        let wf = load(SINGLE_WITH_OUTPUT);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "do_call");
        let mut ctx = new_ctx(json!({ "text": "test" }), &BTreeMap::new());

        let mut responses = BTreeMap::new();
        responses.insert("callee".into(), json!({ "result": "done", "ok": true }));
        let executor = FakeFuncExecutor::new(responses);

        let out = run_blocking(execute_call_block(
            &wf, func, "do_call", &block, &mut ctx, &executor,
        ))
        .expect("execute");

        assert_eq!(out, json!({ "summary": "done", "flag": true }));
    }

    // ---- T12: Function executor error propagates --------------------------

    #[test]
    fn function_executor_error_propagates() {
        let wf = load(SINGLE_CALL);
        let func = wf.document().functions.get("caller").unwrap();
        let block = get_call_block(&wf, "caller", "do_call");
        let mut ctx = new_ctx(json!({ "user_text": "hi", "n": 1 }), &BTreeMap::new());

        // No response for "callee" → the fake executor returns an error.
        let executor = FakeFuncExecutor::new(BTreeMap::new());

        let err = run_blocking(execute_call_block(
            &wf, func, "do_call", &block, &mut ctx, &executor,
        ))
        .expect_err("should error");

        assert!(matches!(err, MechError::WorkflowValidation { .. }));
    }

    // ---- T13: validate_function_exists accepts/rejects functions -----------

    #[test]
    fn validate_function_exists_rejects_missing_function() {
        let wf = load(SINGLE_CALL);
        let err = validate_function_exists(&wf, "ghost", "do_call").expect_err("must reject ghost");
        match err {
            MechError::WorkflowValidation { errors } => {
                assert!(
                    errors.iter().any(|e| e.contains("ghost")),
                    "error should mention `ghost`: {errors:?}"
                );
            }
            other => panic!("expected WorkflowValidation, got {other:?}"),
        }
    }

    #[test]
    fn validate_function_exists_passes_for_declared_function() {
        let wf = load(SINGLE_CALL);
        validate_function_exists(&wf, "callee", "do_call")
            .expect("callee exists and must be accepted");
    }
}
