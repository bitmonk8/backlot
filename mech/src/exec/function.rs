//! Function executor (Deliverable 12).
//!
//! [`FunctionRunner`] executes a single named function within a workflow:
//!
//! 1. Look up the function definition.
//! 2. Create a fresh [`ExecutionContext`] (new function context from
//!    declarations, shared [`WorkflowState`]).
//! 3. Detect execution mode (imperative vs dataflow) from the function's
//!    block edges.
//! 4. Dispatch to [`run_function_imperative`] or [`run_function_dataflow`].
//! 5. Return the terminal block's output.
//!
//! `FunctionRunner` implements [`FunctionExecutor`] so call blocks can invoke
//! it recursively. A depth counter prevents unbounded recursion.

use serde_json::Value as JsonValue;

use crate::context::{ExecutionContext, WorkflowState};
use crate::error::{MechError, MechResult};
use crate::exec::BoxFuture;
use crate::exec::agent::AgentExecutor;
use crate::exec::call::FunctionExecutor;
use crate::exec::dataflow::run_function_dataflow;
use crate::exec::schedule::run_function_imperative;
use crate::loader::Workflow;
use crate::schema::{BlockDef, FunctionDef};

/// Default maximum call depth to prevent infinite recursion.
const DEFAULT_MAX_DEPTH: usize = 64;

/// Execution mode for a function, detected from its block edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Blocks connected by transitions (control edges).
    Imperative,
    /// Blocks connected only by `depends_on` (data edges).
    Dataflow,
}

/// Detect execution mode from a function's blocks.
///
/// If any block declares outgoing transitions, use imperative mode (which
/// also handles `depends_on` on individual blocks as readiness gates). If no
/// block has transitions but some have `depends_on`, use dataflow mode.
/// Single-block or unconnected functions default to imperative (the entry
/// block finder handles them correctly).
pub fn detect_mode(function: &FunctionDef) -> ExecutionMode {
    let has_transitions = function.blocks.values().any(|b| {
        let transitions = match b {
            BlockDef::Prompt(p) => &p.transitions,
            BlockDef::Call(c) => &c.transitions,
        };
        !transitions.is_empty()
    });

    if has_transitions {
        return ExecutionMode::Imperative;
    }

    let has_depends = function.blocks.values().any(|b| {
        let deps = match b {
            BlockDef::Prompt(p) => &p.depends_on,
            BlockDef::Call(c) => &c.depends_on,
        };
        !deps.is_empty()
    });

    if has_depends {
        ExecutionMode::Dataflow
    } else {
        ExecutionMode::Imperative
    }
}

/// Runs functions within a workflow, handling recursive calls via
/// [`FunctionExecutor`].
///
/// Each invocation creates a fresh [`ExecutionContext`] with its own function
/// context (from declarations) and the shared [`WorkflowState`]. The depth
/// counter is incremented for each nested call; exceeding `max_depth`
/// returns an error.
pub struct FunctionRunner<'w> {
    workflow: &'w Workflow,
    agent_executor: &'w dyn AgentExecutor,
    workflow_state: WorkflowState,
    max_depth: usize,
    current_depth: usize,
}

impl<'w> FunctionRunner<'w> {
    /// Create a top-level runner (depth 0).
    pub fn new(
        workflow: &'w Workflow,
        agent_executor: &'w dyn AgentExecutor,
        workflow_state: WorkflowState,
    ) -> Self {
        Self {
            workflow,
            agent_executor,
            workflow_state,
            max_depth: DEFAULT_MAX_DEPTH,
            current_depth: 0,
        }
    }

    /// Override the maximum call depth (default: 64).
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Run a named function with the given input.
    pub async fn run_function(
        &self,
        function_name: &str,
        input: JsonValue,
    ) -> MechResult<JsonValue> {
        let function = self
            .workflow
            .file()
            .functions
            .get(function_name)
            .ok_or_else(|| MechError::Validation {
                errors: vec![format!("function `{function_name}` not found in workflow")],
            })?;

        let meta = serde_json::json!({
            "run_id": "default",
            "function": function_name,
            "depth": self.current_depth,
        });

        let mut ctx =
            ExecutionContext::new(input, meta, &function.context, self.workflow_state.clone())?;

        self.run_function_with_ctx(function_name, function, &mut ctx)
            .await
    }

    /// Run a function with an externally-provided execution context.
    /// Used internally and by tests that need to inspect the context afterward.
    async fn run_function_with_ctx(
        &self,
        function_name: &str,
        function: &FunctionDef,
        ctx: &mut ExecutionContext,
    ) -> MechResult<JsonValue> {
        let mode = detect_mode(function);

        match mode {
            ExecutionMode::Imperative => {
                run_function_imperative(
                    self.workflow,
                    function_name,
                    function,
                    ctx,
                    self.agent_executor,
                    self,
                )
                .await
            }
            ExecutionMode::Dataflow => {
                run_function_dataflow(
                    self.workflow,
                    function_name,
                    function,
                    ctx,
                    self.agent_executor,
                    self,
                )
                .await
            }
        }
    }

    /// Create a child runner for nested function calls.
    fn child(&self) -> Self {
        Self {
            workflow: self.workflow,
            agent_executor: self.agent_executor,
            workflow_state: self.workflow_state.clone(),
            max_depth: self.max_depth,
            current_depth: self.current_depth + 1,
        }
    }
}

impl FunctionExecutor for FunctionRunner<'_> {
    fn call<'a>(
        &'a self,
        function_name: &'a str,
        input: JsonValue,
    ) -> BoxFuture<'a, Result<JsonValue, MechError>> {
        Box::pin(async move {
            if self.current_depth >= self.max_depth {
                return Err(MechError::Validation {
                    errors: vec![format!(
                        "maximum call depth ({}) exceeded calling `{function_name}`",
                        self.max_depth
                    )],
                });
            }
            let child = self.child();
            child.run_function(function_name, input).await
        })
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::agent::{AgentExecutor, AgentRequest, AgentResponse};
    use crate::loader::WorkflowLoader;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    struct SequentialAgent {
        responses: Mutex<Vec<JsonValue>>,
    }

    impl SequentialAgent {
        fn new(responses: Vec<JsonValue>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    impl AgentExecutor for SequentialAgent {
        fn run<'a>(
            &'a self,
            _request: AgentRequest,
        ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
            let output = self.responses.lock().unwrap().remove(0);
            Box::pin(async move { Ok(AgentResponse { output }) })
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

    // ---- T1: Imperative single function, linear flow ----------------------

    #[test]
    fn imperative_linear_flow() {
        let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      a:
        prompt: "step a"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - goto: b
      b:
        prompt: "step b"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;
        let wf = load(yaml);
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let agent = SequentialAgent::new(vec![json!({ "val": "A" }), json!({ "result": "B" })]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let out = run_blocking(runner.run_function("main", json!({}))).unwrap();
        assert_eq!(out, json!({ "result": "B" }));
    }

    // ---- T2: Function calls another function via call block ---------------

    #[test]
    fn function_calls_another_function() {
        let yaml = r#"
functions:
  outer:
    input: { type: object }
    blocks:
      step1:
        prompt: "classify"
        schema:
          type: object
          required: [category]
          properties: { category: { type: string } }
        transitions:
          - goto: step2
      step2:
        call: inner
        input:
          data: "{{input.text}}"
  inner:
    input: { type: object }
    blocks:
      process:
        prompt: "process {{input.data}}"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;
        let wf = load(yaml);
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        // outer.step1 uses first response, inner.process uses second.
        let agent = SequentialAgent::new(vec![
            json!({ "category": "tech" }),
            json!({ "result": "processed" }),
        ]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let out = run_blocking(runner.run_function("outer", json!({ "text": "hello" }))).unwrap();
        assert_eq!(out, json!({ "result": "processed" }));
    }

    // ---- T3: Recursive calls respect depth limit --------------------------

    #[test]
    fn recursive_calls_hit_depth_limit() {
        let yaml = r#"
functions:
  recurse:
    input: { type: object }
    output:
      type: object
    blocks:
      step:
        call: recurse
        input:
          x: "{{input.x}}"
"#;
        let wf = load(yaml);
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let agent = SequentialAgent::new(vec![]);
        let runner = FunctionRunner::new(&wf, &agent, ws).with_max_depth(3);

        let err = run_blocking(runner.run_function("recurse", json!({ "x": 1 }))).unwrap_err();
        match err {
            MechError::Validation { errors } => {
                assert!(
                    errors[0].contains("maximum call depth"),
                    "expected depth error, got: {}",
                    errors[0]
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    // ---- T4: Function context is fresh per invocation ---------------------

    #[test]
    fn function_context_fresh_per_invocation() {
        let yaml = r#"
functions:
  caller:
    input: { type: object }
    blocks:
      call1:
        call: counter
        input: {}
        transitions:
          - goto: call2
      call2:
        call: counter
        input: {}
  counter:
    input: { type: object }
    context:
      count: { type: integer, initial: 0 }
    blocks:
      step:
        prompt: "count is {{context.count}}"
        schema:
          type: object
          required: [val]
          properties: { val: { type: integer } }
        set_context:
          count: "context.count + 1"
"#;
        let wf = load(yaml);
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        // First call to counter returns 1, second call to counter also returns
        // 1 (fresh context each time, starting from 0, incremented to 1).
        let agent = SequentialAgent::new(vec![json!({ "val": 1 }), json!({ "val": 1 })]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let out = run_blocking(runner.run_function("caller", json!({}))).unwrap();
        // Output is from the second call block (terminal).
        assert_eq!(out, json!({ "val": 1 }));
    }

    // ---- T5: Workflow context shared across invocations --------------------

    #[test]
    fn workflow_context_shared_across_invocations() {
        let yaml = r#"
workflow:
  context:
    total: { type: integer, initial: 0 }
functions:
  caller:
    input: { type: object }
    blocks:
      call1:
        call: incrementer
        input: {}
        transitions:
          - goto: call2
      call2:
        call: incrementer
        input: {}
  incrementer:
    input: { type: object }
    blocks:
      step:
        prompt: "current total: {{workflow.total}}"
        schema:
          type: object
          required: [val]
          properties: { val: { type: integer } }
        set_workflow:
          total: "workflow.total + 1"
"#;
        let wf = load(yaml);
        let wf_decls = wf
            .file()
            .workflow
            .as_ref()
            .map(|w| &w.context)
            .cloned()
            .unwrap_or_default();
        let ws = WorkflowState::from_declarations(&wf_decls).unwrap();
        let agent = SequentialAgent::new(vec![json!({ "val": 1 }), json!({ "val": 2 })]);
        let runner = FunctionRunner::new(&wf, &agent, ws.clone());

        run_blocking(runner.run_function("caller", json!({}))).unwrap();
        // Both incrementer calls wrote to workflow.total: 0→1→2.
        assert_eq!(ws.get("total"), Some(json!(2)));
    }

    // ---- T6: Dataflow function via FunctionRunner -------------------------

    #[test]
    fn dataflow_function_via_runner() {
        let yaml = r#"
functions:
  main:
    input: { type: object }
    output:
      type: object
      required: [result]
      properties: { result: { type: string } }
    blocks:
      a:
        prompt: "root"
        schema:
          type: object
          required: [x]
          properties: { x: { type: integer } }
      b:
        prompt: "leaf {{block.a.output.x}}"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
        depends_on: [a]
"#;
        let wf = load(yaml);
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let agent = SequentialAgent::new(vec![json!({ "x": 42 }), json!({ "result": "done" })]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let out = run_blocking(runner.run_function("main", json!({}))).unwrap();
        assert_eq!(out, json!({ "result": "done" }));
    }

    // ---- T7: Mode detection -----------------------------------------------

    #[test]
    fn mode_detection() {
        // Imperative: has transitions.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        transitions:
          - goto: b
      b:
        prompt: "b"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;
        let wf = load(yaml);
        assert_eq!(
            detect_mode(wf.file().functions.get("f").unwrap()),
            ExecutionMode::Imperative
        );

        // Dataflow: has depends_on, no transitions.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
      b:
        prompt: "b"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        depends_on: [a]
"#;
        let wf = load(yaml);
        assert_eq!(
            detect_mode(wf.file().functions.get("f").unwrap()),
            ExecutionMode::Dataflow
        );

        // Single block, no edges: imperative (default).
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;
        let wf = load(yaml);
        assert_eq!(
            detect_mode(wf.file().functions.get("f").unwrap()),
            ExecutionMode::Imperative
        );
    }

    // ---- T8: Unknown function name errors ---------------------------------

    #[test]
    fn unknown_function_name_errors() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;
        let wf = load(yaml);
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let agent = SequentialAgent::new(vec![]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let err = run_blocking(runner.run_function("nonexistent", json!({}))).unwrap_err();
        match err {
            MechError::Validation { errors } => {
                assert!(errors[0].contains("nonexistent"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    // ---- T9: §12 worked example end-to-end (billing path) -----------------

    const FULL_EXAMPLE: &str = include_str!("../schema/full_example.yaml");

    #[test]
    fn worked_example_billing_path() {
        let wf = load(FULL_EXAMPLE);
        let wf_decls = wf
            .file()
            .workflow
            .as_ref()
            .map(|w| &w.context)
            .cloned()
            .unwrap_or_default();
        let ws = WorkflowState::from_declarations(&wf_decls).unwrap();

        // Billing path: classify → billing (call resolve_billing) → respond.
        // Blocks executed:
        //   support_triage.classify → output.category == "billing"
        //   support_triage.billing → call resolve_billing
        //     resolve_billing.analyze → prompt block
        //     resolve_billing.resolve → prompt block (depends_on: [analyze])
        //   support_triage.respond → prompt block
        let agent = SequentialAgent::new(vec![
            // support_triage.classify
            json!({ "category": "billing", "urgency": "high" }),
            // resolve_billing.analyze
            json!({ "root_cause": "overcharge", "resolution_action": "refund" }),
            // resolve_billing.resolve
            json!({ "resolved": true, "summary": "Refunded $50" }),
            // support_triage.respond
            json!({ "resolved": true, "summary": "Your billing issue has been resolved." }),
        ]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let out = run_blocking(runner.run_function(
            "support_triage",
            json!({
                "ticket_text": "I was overcharged",
                "customer_tier": "pro"
            }),
        ))
        .unwrap();

        assert_eq!(out["resolved"], json!(true));
        assert!(out["summary"].as_str().unwrap().contains("resolved"));
    }

    // ---- T10: §12 worked example technical path with self-loop ------------

    #[test]
    fn worked_example_technical_path() {
        let wf = load(FULL_EXAMPLE);
        let wf_decls = wf
            .file()
            .workflow
            .as_ref()
            .map(|w| &w.context)
            .cloned()
            .unwrap_or_default();
        let ws = WorkflowState::from_declarations(&wf_decls).unwrap();

        // Technical path: classify → technical (self-loop, attempts increments)
        // First attempt: no steps → loop back. Second attempt: has steps → respond.
        let agent = SequentialAgent::new(vec![
            // support_triage.classify
            json!({ "category": "technical", "urgency": "medium" }),
            // support_triage.technical (attempt 1: no steps)
            json!({ "diagnosis": "investigating", "steps": [] }),
            // support_triage.technical (attempt 2: has steps)
            json!({ "diagnosis": "found it", "steps": ["reboot", "update"] }),
            // support_triage.respond
            json!({ "resolved": true, "summary": "Technical issue resolved." }),
        ]);
        let runner = FunctionRunner::new(&wf, &agent, ws);

        let out = run_blocking(runner.run_function(
            "support_triage",
            json!({
                "ticket_text": "App crashes on startup",
                "customer_tier": "enterprise"
            }),
        ))
        .unwrap();

        assert_eq!(out["resolved"], json!(true));
    }
}
