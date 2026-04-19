//! End-to-end integration tests (Deliverable 16).
//!
//! Every test loads a real YAML workflow via `WorkflowLoader::load_str`,
//! runs it through `WorkflowRuntime`, and asserts on output and intermediate
//! state. All tests are hermetic: deterministic fake agents, no network, no
//! filesystem temp dirs.

use std::sync::{Arc, Mutex};

use serde_json::{Value as JsonValue, json};

use mech::{
    AgentExecutor, AgentRequest, AgentResponse, BoxFuture, MechError, MechStore, Workflow,
    WorkflowLoader, WorkflowRuntime,
};

// ---- Test helpers ---------------------------------------------------------

fn load(yaml: &str) -> Workflow {
    WorkflowLoader::new().load_str(yaml).expect("load failed")
}

fn run_blocking<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

/// Returns canned responses in order. Panics if exhausted.
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

/// Captures every request and delegates to a SequentialAgent.
struct RecordingAgent {
    requests: Arc<Mutex<Vec<AgentRequest>>>,
    inner: SequentialAgent,
}

impl RecordingAgent {
    fn new(responses: Vec<JsonValue>) -> (Self, Arc<Mutex<Vec<AgentRequest>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let agent = Self {
            requests: Arc::clone(&log),
            inner: SequentialAgent::new(responses),
        };
        (agent, log)
    }
}

impl AgentExecutor for RecordingAgent {
    fn run<'a>(&'a self, request: AgentRequest) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.run(request)
    }
}

const FULL_EXAMPLE: &str = include_str!("../testdata/full_example.yaml");

#[test]
fn worked_example_general_path() {
    let wf = load(FULL_EXAMPLE);
    // classify → general (terminal, no transitions).
    let agent = SequentialAgent::new(vec![
        json!({ "category": "other", "urgency": "low" }),
        json!({ "resolved": true, "summary": "Here is the info." }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run(
        "support_triage",
        json!({ "ticket_text": "How do I reset?", "customer_tier": "free" }),
    ))
    .unwrap();
    assert_eq!(out["resolved"], json!(true));
    assert!(out["summary"].is_string());
}

#[test]
fn worked_example_billing_path() {
    let wf = load(FULL_EXAMPLE);
    // classify → billing (call resolve_billing: analyze→resolve dataflow) → respond.
    let agent = SequentialAgent::new(vec![
        json!({ "category": "billing", "urgency": "high" }),
        json!({ "root_cause": "overcharge", "resolution_action": "refund" }),
        json!({ "resolved": true, "summary": "Refunded $50" }),
        json!({ "resolved": true, "summary": "Billing resolved." }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run(
        "support_triage",
        json!({ "ticket_text": "Overcharged", "customer_tier": "pro" }),
    ))
    .unwrap();
    assert_eq!(out["resolved"], json!(true));
}

#[test]
fn worked_example_technical_self_loop() {
    let wf = load(FULL_EXAMPLE);
    // classify → technical (empty steps, attempts=1 < 3 → self-loop)
    //          → technical (has steps → respond)
    let agent = SequentialAgent::new(vec![
        json!({ "category": "technical", "urgency": "medium" }),
        json!({ "diagnosis": "investigating", "steps": [] }),
        json!({ "diagnosis": "found it", "steps": ["reboot"] }),
        json!({ "resolved": true, "summary": "Fixed." }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run(
        "support_triage",
        json!({ "ticket_text": "Crash on startup", "customer_tier": "enterprise" }),
    ))
    .unwrap();
    assert_eq!(out["resolved"], json!(true));
}

#[test]
fn worked_example_technical_escalation() {
    let wf = load(FULL_EXAMPLE);
    // classify → technical (×3, always empty steps, attempts reaches 3) → escalate
    let agent = SequentialAgent::new(vec![
        json!({ "category": "technical", "urgency": "high" }),
        json!({ "diagnosis": "unknown", "steps": [] }), // attempts=1
        json!({ "diagnosis": "unknown", "steps": [] }), // attempts=2
        json!({ "diagnosis": "unknown", "steps": [] }), // attempts=3, guard context.attempts < 3 is false
        json!({ "notice": "Escalated to engineering.", "suggested_team": "platform" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run(
        "support_triage",
        json!({ "ticket_text": "Total failure", "customer_tier": "enterprise" }),
    ))
    .unwrap();
    assert!(out["notice"].is_string());
    assert!(out["suggested_team"].is_string());
}

// ---- Recursive function calls ---------------------------------------------

const RECURSIVE_WORKFLOW: &str = r#"
functions:
  countdown:
    input:
      type: object
      required: [n]
      properties:
        n: { type: integer }
    output:
      type: object
      required: [result]
      properties:
        result: { type: string }
    blocks:
      check:
        prompt: "n is {{input.n}}"
        schema:
          type: object
          required: [done, result]
          properties:
            done: { type: boolean }
            result: { type: string }
        transitions:
          - when: 'output.done == true'
            goto: finish
          - goto: recurse
      recurse:
        call: countdown
        input:
          n: "{{input.n - 1}}"
      finish:
        prompt: "done"
        schema:
          type: object
          required: [result]
          properties:
            result: { type: string }
"#;

#[test]
fn recursive_function_terminates_on_guard() {
    let wf = load(RECURSIVE_WORKFLOW);
    // depth 0: check (done=false) → recurse → depth 1: check (done=true) → finish
    let agent = SequentialAgent::new(vec![
        json!({ "done": false, "result": "not yet" }),
        json!({ "done": true, "result": "base case" }),
        json!({ "result": "final" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run("countdown", json!({ "n": 2 }))).unwrap();
    assert_eq!(out["result"], json!("final"));
}

#[test]
fn recursive_function_depth_limit() {
    let wf = load(RECURSIVE_WORKFLOW);
    // Every level returns done=false, triggering infinite recursion.
    // Provide enough responses to exceed depth 2.
    let agent = SequentialAgent::new(vec![
        json!({ "done": false, "result": "x" }),
        json!({ "done": false, "result": "x" }),
        json!({ "done": false, "result": "x" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent).with_max_depth(2);
    let err = run_blocking(rt.run("countdown", json!({ "n": 100 }))).unwrap_err();
    match err {
        MechError::ExecutionInvariant { message } => {
            assert!(
                message.contains("maximum call depth"),
                "expected depth error, got: {message}"
            );
        }
        other => panic!("expected ExecutionInvariant(depth), got {other:?}"),
    }
}

// ---- Dataflow with shared dependencies (diamond) --------------------------

const DIAMOND_DATAFLOW: &str = r#"
functions:
  analyze:
    input:
      type: object
      required: [text]
      properties:
        text: { type: string }
    output:
      type: object
      required: [report]
      properties:
        report: { type: string }
    terminals: [synthesize]
    blocks:
      extract:
        prompt: "extract from {{input.text}}"
        schema:
          type: object
          required: [facts]
          properties:
            facts: { type: array, items: { type: string } }

      score:
        prompt: "score {{block.extract.output.facts}}"
        schema:
          type: object
          required: [scores]
          properties:
            scores: { type: array, items: { type: number } }
        depends_on: [extract]

      classify:
        prompt: "classify {{block.extract.output.facts}}"
        schema:
          type: object
          required: [labels]
          properties:
            labels: { type: array, items: { type: string } }
        depends_on: [extract]

      synthesize:
        prompt: "synthesize scores={{block.score.output.scores}} labels={{block.classify.output.labels}}"
        schema:
          type: object
          required: [report]
          properties:
            report: { type: string }
        depends_on: [score, classify]
"#;

#[test]
fn dataflow_diamond_runs_extract_once() {
    let wf = load(DIAMOND_DATAFLOW);
    // 4 blocks, 4 agent calls: extract, then classify+score (alphabetical within level), then synthesize.
    let (agent, requests) = RecordingAgent::new(vec![
        json!({ "facts": ["fact1", "fact2"] }),
        json!({ "labels": ["positive", "neutral"] }),
        json!({ "scores": [0.9, 0.5] }),
        json!({ "report": "Analysis complete." }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run("analyze", json!({ "text": "test input" }))).unwrap();
    assert_eq!(out["report"], json!("Analysis complete."));
    assert_eq!(requests.lock().unwrap().len(), 4, "exactly 4 agent calls");
}

#[test]
fn dataflow_diamond_output_incorporates_intermediates() {
    let wf = load(DIAMOND_DATAFLOW);
    let (agent, requests) = RecordingAgent::new(vec![
        json!({ "facts": ["A", "B"] }),
        json!({ "labels": ["x", "y"] }),
        json!({ "scores": [1.0, 2.0] }),
        json!({ "report": "done" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("analyze", json!({ "text": "t" }))).unwrap();

    let reqs = requests.lock().unwrap();
    // synthesize prompt should reference scores and labels.
    let synth_prompt = &reqs[3].prompt;
    assert!(
        synth_prompt.contains("1") && synth_prompt.contains("2"),
        "synthesize prompt should contain scores: {synth_prompt}"
    );
    assert!(
        synth_prompt.contains("x") && synth_prompt.contains("y"),
        "synthesize prompt should contain labels: {synth_prompt}"
    );
}

// ---- Error paths ----------------------------------------------------------

#[test]
fn error_schema_validation_failure() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [count]
          properties:
            count: { type: integer }
"#;
    let wf = load(yaml);
    // Return string instead of integer for `count`.
    let agent = SequentialAgent::new(vec![json!({ "count": "not-an-int" })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let err = run_blocking(rt.run("main", json!({}))).unwrap_err();
    assert!(
        matches!(err, MechError::SchemaValidationFailure { .. }),
        "expected SchemaValidationFailure, got {err:?}"
    );
}

#[test]
fn error_missing_required_field() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [name, age]
          properties:
            name: { type: string }
            age: { type: integer }
"#;
    let wf = load(yaml);
    // Missing `age`.
    let agent = SequentialAgent::new(vec![json!({ "name": "Alice" })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let err = run_blocking(rt.run("main", json!({}))).unwrap_err();
    assert!(matches!(err, MechError::SchemaValidationFailure { .. }));
}

#[test]
fn error_depth_limit_exceeded() {
    let yaml = r#"
functions:
  loop_fn:
    input: { type: object }
    output: { type: object }
    blocks:
      step:
        call: loop_fn
        input:
          x: "{{input.x}}"
"#;
    let wf = load(yaml);
    let agent = SequentialAgent::new(vec![]);
    let rt = WorkflowRuntime::new(&wf, &agent).with_max_depth(2);
    let err = run_blocking(rt.run("loop_fn", json!({ "x": 1 }))).unwrap_err();
    match err {
        MechError::ExecutionInvariant { message } => {
            assert!(message.contains("maximum call depth"));
        }
        other => panic!("expected ExecutionInvariant, got {other:?}"),
    }
}

#[test]
fn error_missing_entry_function() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;
    let wf = load(yaml);
    let agent = SequentialAgent::new(vec![]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let err = run_blocking(rt.run("nonexistent", json!({}))).unwrap_err();
    assert!(matches!(err, MechError::ExecutionInvariant { .. }));
}

#[test]
fn error_llm_call_failure_propagates() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;
    let wf = load(yaml);
    // Agent that always fails with LlmCallFailure.
    struct FailingAgent;
    impl AgentExecutor for FailingAgent {
        fn run<'a>(
            &'a self,
            _request: AgentRequest,
        ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
            Box::pin(async move {
                Err(MechError::LlmCallFailure {
                    block: String::new(),
                    message: "provider returned 500".into(),
                })
            })
        }
    }
    let agent = FailingAgent;
    let rt = WorkflowRuntime::new(&wf, &agent);
    let err = run_blocking(rt.run("main", json!({}))).unwrap_err();
    match err {
        MechError::LlmCallFailure { block, message } => {
            assert_eq!(block, "step");
            assert!(message.contains("500"));
        }
        other => panic!("expected LlmCallFailure, got {other:?}"),
    }
}

// ---- Cue-orchestrated execution -------------------------------------------

struct EventLog {
    log: Arc<Mutex<Vec<cue::CueEvent>>>,
}

impl EventLog {
    fn new() -> (Self, Arc<Mutex<Vec<cue::CueEvent>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                log: Arc::clone(&log),
            },
            log,
        )
    }
}

impl traits::EventEmitter<cue::CueEvent> for EventLog {
    fn emit(&self, event: cue::CueEvent) {
        self.log.lock().unwrap().push(event);
    }
}

#[test]
fn cue_orchestrated_success() {
    let wf = Arc::new(load(FULL_EXAMPLE));
    let agent: Arc<dyn AgentExecutor> = Arc::new(SequentialAgent::new(vec![
        json!({ "category": "other", "urgency": "low" }),
        json!({ "resolved": true, "summary": "Done." }),
    ]));
    let mut store = MechStore::new().with_agent(agent);
    let root_id = store.create_root(
        "triage",
        Arc::clone(&wf),
        "support_triage",
        json!({ "ticket_text": "Question", "customer_tier": "free" }),
    );
    let (emitter, events) = EventLog::new();
    let mut orch = cue::Orchestrator::new(store, emitter);
    let outcome = run_blocking(orch.run(root_id)).unwrap();
    assert_eq!(outcome, cue::TaskOutcome::Success);
    assert!(events.lock().unwrap().iter().any(|e| matches!(
        e,
        cue::CueEvent::TaskCompleted {
            outcome: cue::TaskOutcome::Success,
            ..
        }
    )));
}

#[test]
fn cue_orchestrated_failure() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [value]
          properties:
            value: { type: integer }
"#;
    let wf = Arc::new(load(yaml));
    // Return wrong type to trigger schema validation failure.
    let agent: Arc<dyn AgentExecutor> =
        Arc::new(SequentialAgent::new(vec![json!({ "value": "string" })]));
    let mut store = MechStore::new().with_agent(agent);
    let root_id = store.create_root("fail", Arc::clone(&wf), "main", json!({}));
    let (emitter, _events) = EventLog::new();
    let mut orch = cue::Orchestrator::new(store, emitter);
    let outcome = run_blocking(orch.run(root_id)).unwrap();
    assert!(
        matches!(outcome, cue::TaskOutcome::Failed { .. }),
        "expected Failed, got {outcome:?}"
    );
}

// ---- Workflow state shared across function calls --------------------------

#[test]
fn workflow_state_shared_across_calls() {
    let yaml = r#"
workflow:
  context:
    counter: { type: integer, initial: 0 }
functions:
  main:
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
        prompt: "counter={{workflow.counter}}"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
        set_workflow:
          counter: "workflow.counter + 1"
"#;
    let wf = load(yaml);
    let agent = SequentialAgent::new(vec![json!({ "ok": true }), json!({ "ok": true })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let (_out, ws) = run_blocking(rt.run_with_state("main", json!({}))).unwrap();
    // Two calls to incrementer: 0→1→2.
    assert_eq!(ws.get("counter"), Some(json!(2)));
}

#[test]
fn workflow_state_multiple_variables() {
    let yaml = r#"
workflow:
  context:
    sum: { type: integer, initial: 0 }
    calls: { type: integer, initial: 0 }
functions:
  main:
    input: { type: object }
    blocks:
      a:
        prompt: "step a"
        schema:
          type: object
          required: [value]
          properties:
            value: { type: integer }
        set_workflow:
          sum: "workflow.sum + output.value"
          calls: "workflow.calls + 1"
        transitions:
          - goto: b
      b:
        prompt: "step b"
        schema:
          type: object
          required: [value]
          properties:
            value: { type: integer }
        set_workflow:
          sum: "workflow.sum + output.value"
          calls: "workflow.calls + 1"
"#;
    let wf = load(yaml);
    let agent = SequentialAgent::new(vec![json!({ "value": 10 }), json!({ "value": 20 })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let (_out, ws) = run_blocking(rt.run_with_state("main", json!({}))).unwrap();
    assert_eq!(ws.get("sum"), Some(json!(30)));
    assert_eq!(ws.get("calls"), Some(json!(2)));
}

// ---- Conversation history scoping -----------------------------------------

#[test]
fn conversation_history_accumulates_in_imperative_mode() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      a:
        prompt: "first"
        schema:
          type: object
          required: [x]
          properties:
            x: { type: string }
        transitions:
          - goto: b
      b:
        prompt: "second"
        schema:
          type: object
          required: [y]
          properties:
            y: { type: string }
        transitions:
          - goto: c
      c:
        prompt: "third"
        schema:
          type: object
          required: [z]
          properties:
            z: { type: string }
"#;
    let wf = load(yaml);
    let (agent, requests) = RecordingAgent::new(vec![
        json!({ "x": "A" }),
        json!({ "y": "B" }),
        json!({ "z": "C" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({}))).unwrap();

    let reqs = requests.lock().unwrap();
    // Block a: no prior history.
    assert_eq!(reqs[0].history.len(), 0, "block a should see empty history");
    // Block b: sees a's user+assistant = 2 messages.
    assert_eq!(reqs[1].history.len(), 2, "block b should see 2 messages");
    // Block c: sees a+b's messages = 4 messages.
    assert_eq!(reqs[2].history.len(), 4, "block c should see 4 messages");
}

#[test]
fn call_block_callee_gets_fresh_conversation() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      a:
        prompt: "main block a"
        schema:
          type: object
          required: [x]
          properties:
            x: { type: string }
        transitions:
          - goto: do_call
      do_call:
        call: sub
        input: {}
        transitions:
          - goto: c
      c:
        prompt: "main block c"
        schema:
          type: object
          required: [z]
          properties:
            z: { type: string }
  sub:
    input: { type: object }
    blocks:
      inner:
        prompt: "sub inner"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;
    let wf = load(yaml);
    let (agent, requests) = RecordingAgent::new(vec![
        json!({ "x": "A" }),   // main.a
        json!({ "ok": true }), // sub.inner
        json!({ "z": "C" }),   // main.c
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({}))).unwrap();

    let reqs = requests.lock().unwrap();
    // main.a: empty history (first block).
    assert_eq!(reqs[0].history.len(), 0);
    // sub.inner: fresh conversation (callee starts empty per §4.6).
    assert_eq!(
        reqs[1].history.len(),
        0,
        "callee should get fresh conversation"
    );
    // main.c: sees main.a's messages (call block is conversation-transparent).
    assert_eq!(
        reqs[2].history.len(),
        2,
        "main.c should see history from main.a only"
    );
}

#[test]
fn self_loop_accumulates_conversation_history() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    context:
      rounds: { type: integer, initial: 0 }
    blocks:
      step:
        prompt: "round {{context.rounds}}"
        schema:
          type: object
          required: [done]
          properties:
            done: { type: boolean }
        set_context:
          rounds: "context.rounds + 1"
        transitions:
          - when: 'output.done == true'
            goto: finish
          - goto: step
      finish:
        prompt: "done"
        schema:
          type: object
          required: [result]
          properties:
            result: { type: string }
"#;
    let wf = load(yaml);
    let (agent, requests) = RecordingAgent::new(vec![
        json!({ "done": false }),
        json!({ "done": false }),
        json!({ "done": true }),
        json!({ "result": "ok" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({}))).unwrap();

    let reqs = requests.lock().unwrap();
    // step round 0: empty history.
    assert_eq!(reqs[0].history.len(), 0);
    // step round 1: sees round 0 (2 messages).
    assert_eq!(reqs[1].history.len(), 2);
    // step round 2: sees rounds 0+1 (4 messages).
    assert_eq!(reqs[2].history.len(), 4);
    // finish: sees all 3 rounds of step (6 messages).
    assert_eq!(reqs[3].history.len(), 6);
}

// ---- Multi-function workflow with shared schemas --------------------------

#[test]
fn shared_schema_ref_across_functions() {
    let yaml = r#"
workflow:
  schemas:
    Result:
      type: object
      required: [status, message]
      properties:
        status: { type: string }
        message: { type: string }
functions:
  main:
    input: { type: object }
    blocks:
      step1:
        prompt: "first"
        schema: "$ref:#Result"
        transitions:
          - goto: step2
      step2:
        call: helper
        input: {}
  helper:
    input: { type: object }
    blocks:
      work:
        prompt: "help"
        schema: "$ref:#Result"
"#;
    let wf = load(yaml);
    let agent = SequentialAgent::new(vec![
        json!({ "status": "ok", "message": "step1 done" }),
        json!({ "status": "ok", "message": "helper done" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run("main", json!({}))).unwrap();
    assert_eq!(out["status"], json!("ok"));
    assert_eq!(out["message"], json!("helper done"));
}

// ---- Agent configuration cascade end-to-end -------------------------------

#[test]
fn agent_cascade_block_overrides_workflow_default() {
    let yaml = r#"
workflow:
  agent:
    model: haiku
    grant: [tools]
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
        agent:
          model: opus
          grant: [write]
          write_paths: [out/]
"#;
    let wf = load(yaml);
    let (agent, requests) = RecordingAgent::new(vec![json!({ "ok": true })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({}))).unwrap();

    let req = &requests.lock().unwrap()[0];
    assert_eq!(req.model.as_deref(), Some("opus"));
    assert_eq!(req.grants, vec!["write".to_string()]);
    assert_eq!(req.write_paths, vec!["out/".to_string()]);
}

#[test]
fn system_prompt_rendered_through_runtime() {
    let yaml = r#"
workflow:
  system: "You help {{input.user}}."
functions:
  main:
    input:
      type: object
      required: [user]
      properties:
        user: { type: string }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;
    let wf = load(yaml);
    let (agent, requests) = RecordingAgent::new(vec![json!({ "ok": true })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({ "user": "Ada" }))).unwrap();

    let req = &requests.lock().unwrap()[0];
    assert_eq!(req.system.as_deref(), Some("You help Ada."));
}
// ---- Context isolation per function invocation ----------------------------

#[test]
fn function_context_is_fresh_per_call() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      first:
        call: worker
        input: {}
        transitions:
          - goto: second
      second:
        call: worker
        input: {}
  worker:
    input: { type: object }
    context:
      seen: { type: integer, initial: 0 }
    blocks:
      step:
        prompt: "seen={{context.seen}}"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
        set_context:
          seen: "context.seen + 1"
"#;
    let wf = load(yaml);
    let (agent, requests) = RecordingAgent::new(vec![json!({ "ok": true }), json!({ "ok": true })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({}))).unwrap();

    let reqs = requests.lock().unwrap();
    // Both worker invocations should start with seen=0 (fresh context).
    assert!(
        reqs[0].prompt.contains("seen=0"),
        "first call: {}",
        reqs[0].prompt
    );
    assert!(
        reqs[1].prompt.contains("seen=0"),
        "second call: {}",
        reqs[1].prompt
    );
}

// ---- Mixed imperative+dataflow in same workflow ---------------------------

#[test]
fn imperative_function_calls_dataflow_function() {
    let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step1:
        prompt: "classify"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
        transitions:
          - goto: step2
      step2:
        call: dataflow_fn
        input:
          cat: "{{block.step1.output.category}}"
  dataflow_fn:
    input: { type: object }
    output:
      type: object
      required: [result]
      properties:
        result: { type: string }
    terminals: [sink]
    blocks:
      source:
        prompt: "source for {{input.cat}}"
        schema:
          type: object
          required: [data]
          properties:
            data: { type: string }
      sink:
        prompt: "sink {{block.source.output.data}}"
        schema:
          type: object
          required: [result]
          properties:
            result: { type: string }
        depends_on: [source]
"#;
    let wf = load(yaml);
    let agent = SequentialAgent::new(vec![
        json!({ "category": "tech" }),
        json!({ "data": "raw" }),
        json!({ "result": "processed" }),
    ]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    let out = run_blocking(rt.run("main", json!({}))).unwrap();
    assert_eq!(out["result"], json!("processed"));
}

// ---- Template rendering across namespaces ---------------------------------

#[test]
fn template_rendering_all_namespaces() {
    let yaml = r#"
workflow:
  context:
    global: { type: string, initial: "G" }
functions:
  main:
    input:
      type: object
      required: [user]
      properties:
        user: { type: string }
    context:
      local: { type: string, initial: "L" }
    blocks:
      first:
        prompt: "first"
        schema:
          type: object
          required: [val]
          properties:
            val: { type: string }
        transitions:
          - goto: second
      second:
        prompt: "u={{input.user}} l={{context.local}} g={{workflow.global}} b={{block.first.output.val}}"
        schema:
          type: object
          required: [ok]
          properties:
            ok: { type: boolean }
"#;
    let wf = load(yaml);
    let (agent, requests) =
        RecordingAgent::new(vec![json!({ "val": "FIRST" }), json!({ "ok": true })]);
    let rt = WorkflowRuntime::new(&wf, &agent);
    run_blocking(rt.run("main", json!({ "user": "Ada" }))).unwrap();

    let prompt = &requests.lock().unwrap()[1].prompt;
    assert!(prompt.contains("u=Ada"), "got: {prompt}");
    assert!(prompt.contains("l=L"), "got: {prompt}");
    assert!(prompt.contains("g=G"), "got: {prompt}");
    assert!(prompt.contains("b=FIRST"), "got: {prompt}");
}
