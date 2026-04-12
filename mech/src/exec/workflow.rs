//! Workflow runtime (Deliverable 12).
//!
//! [`WorkflowRuntime`] is the top-level entry point for executing a mech
//! workflow. It initialises the shared [`WorkflowState`] from
//! `workflow.context` declarations, creates a [`FunctionRunner`], and
//! invokes the designated entry function.

use serde_json::Value as JsonValue;

use crate::context::WorkflowState;
use crate::error::{MechError, MechResult};
use crate::exec::agent::AgentExecutor;
use crate::exec::function::FunctionRunner;
use crate::loader::Workflow;

/// Top-level workflow executor.
///
/// Initialises shared state and dispatches to [`FunctionRunner`] for the
/// entry function.
pub struct WorkflowRuntime<'w> {
    workflow: &'w Workflow,
    agent_executor: &'w dyn AgentExecutor,
    max_depth: usize,
}

impl<'w> WorkflowRuntime<'w> {
    /// Create a runtime for the given workflow and agent executor.
    pub fn new(workflow: &'w Workflow, agent_executor: &'w dyn AgentExecutor) -> Self {
        Self {
            workflow,
            agent_executor,
            max_depth: 64,
        }
    }

    /// Override the maximum call depth for recursive function calls.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Run the workflow starting at the named entry function.
    pub async fn run(&self, entry_function: &str, input: JsonValue) -> MechResult<JsonValue> {
        // Initialise shared workflow state from declarations.
        let wf_context_decls = self
            .workflow
            .file()
            .workflow
            .as_ref()
            .map(|w| &w.context)
            .cloned()
            .unwrap_or_default();
        let ws = WorkflowState::from_declarations(&wf_context_decls)?;

        let runner = FunctionRunner::new(self.workflow, self.agent_executor, ws)
            .with_max_depth(self.max_depth);

        runner.run_function(entry_function, input).await
    }

    /// Run the workflow, returning both the output and the final workflow
    /// state. Useful for tests and callers that need to inspect cross-function
    /// state after execution.
    pub async fn run_with_state(
        &self,
        entry_function: &str,
        input: JsonValue,
    ) -> MechResult<(JsonValue, WorkflowState)> {
        let wf_context_decls = self
            .workflow
            .file()
            .workflow
            .as_ref()
            .map(|w| &w.context)
            .cloned()
            .unwrap_or_default();
        let ws = WorkflowState::from_declarations(&wf_context_decls)?;

        let runner = FunctionRunner::new(self.workflow, self.agent_executor, ws.clone())
            .with_max_depth(self.max_depth);

        let output = runner.run_function(entry_function, input).await?;
        Ok((output, ws))
    }

    /// Detect the entry function if none is specified.
    ///
    /// Returns the first function in declaration order (BTreeMap iteration).
    /// Returns an error if the workflow has no functions.
    pub fn default_entry_function(&self) -> MechResult<&str> {
        self.workflow
            .file()
            .functions
            .keys()
            .next()
            .map(String::as_str)
            .ok_or_else(|| MechError::Validation {
                errors: vec!["workflow has no functions".into()],
            })
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
    use crate::loader::WorkflowLoader;
    use serde_json::json;
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
            Box::pin(async move {
                Ok(AgentResponse {
                    output,
                    messages: vec![],
                })
            })
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

    // ---- T1: End-to-end workflow run --------------------------------------

    #[test]
    fn end_to_end_workflow_run() {
        let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      greet:
        prompt: "hello {{input.name}}"
        schema:
          type: object
          required: [greeting]
          properties: { greeting: { type: string } }
"#;
        let wf = load(yaml);
        let agent = SequentialAgent::new(vec![json!({ "greeting": "Hi Alice!" })]);
        let rt = WorkflowRuntime::new(&wf, &agent);

        let out = run_blocking(rt.run("main", json!({ "name": "Alice" }))).unwrap();
        assert_eq!(out, json!({ "greeting": "Hi Alice!" }));
    }

    // ---- T2: Workflow state accessible after run --------------------------

    #[test]
    fn workflow_state_accessible_after_run() {
        let yaml = r#"
workflow:
  context:
    counter: { type: integer, initial: 0 }
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
        set_workflow:
          counter: "workflow.counter + 1"
"#;
        let wf = load(yaml);
        let agent = SequentialAgent::new(vec![json!({ "ok": true })]);
        let rt = WorkflowRuntime::new(&wf, &agent);

        let (out, ws) = run_blocking(rt.run_with_state("main", json!({}))).unwrap();
        assert_eq!(out, json!({ "ok": true }));
        assert_eq!(ws.get("counter"), Some(json!(1)));
    }

    // ---- T3: Default entry function is first in BTreeMap ------------------

    #[test]
    fn default_entry_function() {
        let yaml = r#"
functions:
  alpha:
    input: { type: object }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
  beta:
    input: { type: object }
    blocks:
      b:
        prompt: "b"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;
        let wf = load(yaml);
        let agent = SequentialAgent::new(vec![]);
        let rt = WorkflowRuntime::new(&wf, &agent);

        // BTreeMap: "alpha" < "beta".
        assert_eq!(rt.default_entry_function().unwrap(), "alpha");
    }

    // ---- T4: Unknown entry function errors --------------------------------

    #[test]
    fn unknown_entry_function_errors() {
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
        let agent = SequentialAgent::new(vec![]);
        let rt = WorkflowRuntime::new(&wf, &agent);

        let err = run_blocking(rt.run("nonexistent", json!({}))).unwrap_err();
        assert!(matches!(err, MechError::Validation { .. }));
    }

    // ---- T5: Multi-function workflow with call block ----------------------

    #[test]
    fn multi_function_workflow() {
        let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      classify:
        prompt: "classify {{input.text}}"
        schema:
          type: object
          required: [category]
          properties: { category: { type: string } }
        transitions:
          - goto: process
      process:
        call: handler
        input:
          data: "{{input.text}}"
  handler:
    input: { type: object }
    blocks:
      handle:
        prompt: "handle {{input.data}}"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;
        let wf = load(yaml);
        let agent = SequentialAgent::new(vec![
            json!({ "category": "tech" }),
            json!({ "result": "handled" }),
        ]);
        let rt = WorkflowRuntime::new(&wf, &agent);

        let out = run_blocking(rt.run("main", json!({ "text": "help me" }))).unwrap();
        assert_eq!(out, json!({ "result": "handled" }));
    }

    // ---- T6: Depth limit via WorkflowRuntime ------------------------------

    #[test]
    fn depth_limit_via_runtime() {
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
        let agent = SequentialAgent::new(vec![]);
        let rt = WorkflowRuntime::new(&wf, &agent).with_max_depth(2);

        let err = run_blocking(rt.run("recurse", json!({ "x": 1 }))).unwrap_err();
        match err {
            MechError::Validation { errors } => {
                assert!(errors[0].contains("maximum call depth"));
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    // ---- T7: §12 worked example end-to-end via WorkflowRuntime -----------

    const FULL_EXAMPLE: &str = include_str!("../schema/full_example.yaml");

    #[test]
    fn worked_example_via_runtime() {
        let wf = load(FULL_EXAMPLE);
        // General path: classify → general (terminal).
        let agent = SequentialAgent::new(vec![
            // support_triage.classify
            json!({ "category": "other", "urgency": "low" }),
            // support_triage.general
            json!({ "resolved": true, "summary": "Here's the info you requested." }),
        ]);
        let rt = WorkflowRuntime::new(&wf, &agent);

        let out = run_blocking(rt.run(
            "support_triage",
            json!({
                "ticket_text": "How do I change my password?",
                "customer_tier": "free"
            }),
        ))
        .unwrap();

        assert_eq!(out["resolved"], json!(true));
    }
}
