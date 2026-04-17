//! Cue integration for mech workflows (Deliverable 14).
//!
//! Implements [`MechTask`] as a [`cue::TaskNode`] and [`MechStore`] as a
//! [`cue::TaskStore`].  A mech function invocation maps to one cue leaf task.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use cue::{
    AssessmentResult, BranchVerifyOutcome, ChildResponse, DecompositionResult, FixBudgetCheck,
    LimitsConfig, Model, OrchestratorError, RecoveryDecision, RecoveryEligibility,
    RegistrationInfo, ResumePoint, ScopeCheck, SessionMeta, SubtaskSpec, TaskId, TaskOutcome,
    TaskPath, TaskPhase, TreeContext,
};

use crate::MechError;
use crate::exec::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture, WorkflowRuntime};
use crate::workflow::Workflow;

struct EscalatingExecutor<'a> {
    inner: &'a dyn AgentExecutor,
    override_model: String,
    workflow_default_model: Option<Model>,
}

impl AgentExecutor for EscalatingExecutor<'_> {
    fn run<'a>(
        &'a self,
        mut request: AgentRequest,
    ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
        let should_override = match &request.model {
            None => true,
            // A block explicitly set the same model as the workflow default — treat as "no block override"
            // and apply escalation.
            Some(m) => self
                .workflow_default_model
                .is_some_and(|d| model_name(d) == m.as_str()),
        };
        if should_override {
            request.model = Some(self.override_model.clone());
        }
        self.inner.run(request)
    }
}

// S2 - returns &'static str; no heap allocation until the single caller that needs String.
fn model_name(m: Model) -> &'static str {
    match m {
        Model::Haiku => "haiku",
        Model::Sonnet => "sonnet",
        Model::Opus => "opus",
    }
}

/// Map a workflow YAML model string to a [`Model`] enum, or `None` if unrecognised.
///
/// Only the abstract names (`"haiku"`, `"sonnet"`, `"opus"`) are mapped. Provider-qualified
/// strings such as `"claude-3-haiku-20240307"` return `None`, which disables escalation
/// detection for that workflow. Use abstract names in `workflow.agent.model` to enable
/// cue model escalation.
fn parse_model(s: &str) -> Option<Model> {
    match s {
        "haiku" => Some(Model::Haiku),
        "sonnet" => Some(Model::Sonnet),
        "opus" => Some(Model::Opus),
        _ => None,
    }
}

/// A [`cue::TaskNode`] that executes a single named mech workflow function.
///
/// `MechTask` is always a leaf: `forced_assessment` always returns
/// `TaskPath::Leaf`, and the branch methods (`verify_branch`, `design_fix`,
/// `handle_checkpoint`, `assess_and_design_recovery`, `decompose`) all panic.
///
/// The agent may be `None` at construction time if the task is inserted into a
/// [`MechStore`] that has been configured via [`MechStore::with_agent`] and
/// [`cue::TaskStore::bind_runtime`] will inject it before execution.
pub struct MechTask {
    id: TaskId,
    parent_id: Option<TaskId>,
    goal: String,
    depth: u32,
    phase: TaskPhase,
    // Required by TaskStore trait; always empty for leaf-only tasks.
    subtask_ids: Vec<TaskId>,
    // Required by TaskStore trait; always empty for leaf-only tasks.
    discoveries: Vec<String>,
    // Required by TaskStore trait; always zero for leaf-only tasks.
    recovery_rounds: u32,
    fix_rounds: u32,
    accumulated_cost_usd: f64,
    assessment_model: Model,
    workflow: Arc<Workflow>,
    function_name: String,
    input: JsonValue,
    agent: Option<Arc<dyn AgentExecutor>>,
    workflow_default_model: Option<Model>,
}

impl MechTask {
    /// Create a new `MechTask`.
    ///
    /// # Parameters
    /// - `id` - unique [`TaskId`] assigned by the store
    /// - `goal` - human-readable description of what this invocation does
    /// - `workflow` - compiled workflow (shared across tasks)
    /// - `function_name` - name of the function inside `workflow` to run
    /// - `input` - JSON input passed to the function
    /// - `agent` - executor that handles LLM calls; may be `None` if the task
    ///   will be inserted into a [`MechStore`] whose agent is injected via
    ///   [`MechStore::with_agent`] + [`cue::TaskStore::bind_runtime`]
    pub fn new(
        id: TaskId,
        goal: impl Into<String>,
        workflow: Arc<Workflow>,
        function_name: impl Into<String>,
        input: JsonValue,
        agent: Option<Arc<dyn AgentExecutor>>,
    ) -> Self {
        let workflow_default_model = workflow
            .document()
            .workflow
            .as_ref()
            .and_then(|w| w.agent.as_ref())
            .and_then(|a| match a {
                crate::schema::AgentConfigRef::Inline(cfg) => {
                    cfg.model.as_deref().and_then(parse_model)
                }
                // LIMITATION: $ref-based workflow-level agent configs cannot be resolved at
                // construction time (no schema registry access here). If the workflow uses
                // `agent: "$ref:#named_config"` at the workflow level, model escalation comparison
                // will be skipped (workflow_default_model = None) and escalation will not apply.
                // Inline `agent: { model: haiku|sonnet|opus }` at the workflow level is fully supported.
                crate::schema::AgentConfigRef::Ref(_) => None,
            });
        Self {
            id,
            parent_id: None,
            goal: goal.into(),
            depth: 0,
            phase: TaskPhase::Pending,
            subtask_ids: Vec::new(),
            discoveries: Vec::new(),
            recovery_rounds: 0,
            fix_rounds: 0,
            accumulated_cost_usd: 0.0,
            // Start from the workflow-configured default model (or Haiku if none configured).
            // When cue calls set_assessment with an escalated model, assessment_model diverges
            // from workflow_default_model and execute_leaf activates EscalatingExecutor.
            assessment_model: workflow_default_model.unwrap_or(Model::Haiku),
            workflow,
            function_name: function_name.into(),
            input,
            agent,
            workflow_default_model,
        }
    }

    /// Inject an agent executor. Called by [`MechStore::bind_runtime`].
    pub fn set_agent(&mut self, agent: Arc<dyn AgentExecutor>) {
        self.agent = Some(agent);
    }
}

impl cue::TaskNode for MechTask {
    fn id(&self) -> TaskId {
        self.id
    }
    fn parent_id(&self) -> Option<TaskId> {
        self.parent_id
    }
    fn goal(&self) -> &str {
        &self.goal
    }
    fn depth(&self) -> u32 {
        self.depth
    }
    fn phase(&self) -> TaskPhase {
        self.phase
    }
    fn subtask_ids(&self) -> &[TaskId] {
        &self.subtask_ids
    }
    fn discoveries(&self) -> &[String] {
        &self.discoveries
    }
    fn recovery_rounds(&self) -> u32 {
        self.recovery_rounds
    }

    fn is_terminal(&self) -> bool {
        matches!(self.phase, TaskPhase::Completed | TaskPhase::Failed)
    }

    fn resume_point(&self) -> ResumePoint {
        match self.phase {
            TaskPhase::Completed => ResumePoint::Terminal(TaskOutcome::Success),
            TaskPhase::Failed => ResumePoint::Terminal(TaskOutcome::Failed {
                reason: "task previously failed".into(),
            }),
            TaskPhase::Executing => ResumePoint::LeafExecuting,
            TaskPhase::Verifying => ResumePoint::LeafVerifying,
            _ => ResumePoint::NeedAssessment,
        }
    }

    fn forced_assessment(&self, _max_depth: u32) -> Option<AssessmentResult> {
        Some(AssessmentResult {
            path: TaskPath::Leaf,
            model: self.assessment_model,
            rationale: "mech tasks are always leaves".into(),
            magnitude: None,
        })
    }

    fn needs_decomposition(&self) -> bool {
        false
    }
    fn decompose_model(&self) -> Model {
        self.assessment_model
    }

    fn registration_info(&self) -> RegistrationInfo {
        RegistrationInfo {
            parent_id: self.parent_id,
            goal: self.goal.clone(),
            depth: self.depth,
            phase: self.phase,
        }
    }

    fn set_phase(&mut self, phase: TaskPhase) {
        self.phase = phase;
    }

    fn set_assessment(
        &mut self,
        _path: TaskPath,
        model: Model,
        _magnitude: Option<cue::Magnitude>,
    ) {
        self.assessment_model = model;
    }

    fn set_decomposition_rationale(&mut self, _rationale: String) {}

    fn set_subtask_ids(&mut self, ids: &[TaskId], append: bool) {
        if append {
            self.subtask_ids.extend_from_slice(ids);
        } else {
            self.subtask_ids = ids.to_vec();
        }
    }

    fn increment_fix_rounds(&mut self) -> u32 {
        self.fix_rounds += 1;
        self.fix_rounds
    }

    fn accumulate_usage(&mut self, meta: &SessionMeta) -> f64 {
        self.accumulated_cost_usd += meta.cost_usd;
        self.accumulated_cost_usd
    }

    async fn execute_leaf(&mut self, _ctx: &TreeContext) -> TaskOutcome {
        let agent = match &self.agent {
            Some(a) => Arc::clone(a),
            None => panic!(
                "MechTask::execute_leaf: agent not bound — call bind_runtime or pass agent at construction"
            ),
        };

        // Use escalation executor if assessment selected a model that differs from the workflow default.
        // This fires when cue calls set_assessment with an escalated model (Sonnet or Opus).
        let use_escalation = match self.workflow_default_model {
            None => false, // No comparable workflow default; can't detect escalation
            Some(default_model) => self.assessment_model != default_model,
        };

        let result = if use_escalation {
            let escalating = EscalatingExecutor {
                inner: agent.as_ref(),
                override_model: model_name(self.assessment_model).to_owned(),
                workflow_default_model: self.workflow_default_model,
            };
            WorkflowRuntime::new(&self.workflow, &escalating)
                .run(&self.function_name, self.input.clone())
                .await
        } else {
            WorkflowRuntime::new(&self.workflow, agent.as_ref())
                .run(&self.function_name, self.input.clone())
                .await
        };

        match result {
            Ok(_output) => TaskOutcome::Success,
            Err(e) => TaskOutcome::Failed {
                reason: match &e {
                    MechError::LlmCallFailure { .. } | MechError::Timeout { .. } => {
                        format!("__agent_error__: {e}")
                    }
                    _ => e.to_string(),
                },
            },
        }
    }

    async fn verify_branch(
        &mut self,
        _ctx: &TreeContext,
    ) -> Result<BranchVerifyOutcome, OrchestratorError> {
        panic!(
            "MechTask::verify_branch: mech tasks are always leaves — this method must not be called"
        )
    }

    fn fix_round_budget_check(&self, _limits: &LimitsConfig) -> FixBudgetCheck {
        FixBudgetCheck::Exhausted
    }

    async fn check_branch_scope(&self) -> ScopeCheck {
        ScopeCheck::WithinBounds
    }

    async fn design_fix(
        &mut self,
        _ctx: &TreeContext,
        _failure_reason: &str,
        _round: u32,
        _model: Model,
    ) -> Result<Result<Vec<SubtaskSpec>, String>, OrchestratorError> {
        panic!(
            "MechTask::design_fix: mech tasks are always leaves — this method must not be called"
        )
    }

    async fn handle_checkpoint(
        &mut self,
        _ctx: &TreeContext,
        _discoveries: &[String],
    ) -> Result<ChildResponse, OrchestratorError> {
        panic!(
            "MechTask::handle_checkpoint: mech tasks are always leaves — this method must not be called"
        )
    }

    fn can_attempt_recovery(&self, _limits: &LimitsConfig) -> RecoveryEligibility {
        RecoveryEligibility::NotEligible {
            reason: "mech tasks do not support recovery".into(),
        }
    }

    async fn assess_and_design_recovery(
        &mut self,
        _ctx: &TreeContext,
        _failure_reason: &str,
        _round: u32,
    ) -> Result<RecoveryDecision, OrchestratorError> {
        panic!(
            "MechTask::assess_and_design_recovery: mech tasks are always leaves — this method must not be called"
        )
    }

    async fn assess(&mut self, _ctx: &TreeContext) -> Result<AssessmentResult, OrchestratorError> {
        Ok(AssessmentResult {
            path: TaskPath::Leaf,
            model: self.assessment_model,
            rationale: "mech tasks are always leaves".into(),
            magnitude: None,
        })
    }

    async fn decompose(
        &mut self,
        _ctx: &TreeContext,
        _model: Model,
    ) -> Result<DecompositionResult, OrchestratorError> {
        panic!("MechTask::decompose: mech tasks are always leaves — this method must not be called")
    }
}

/// A leaf-only [`cue::TaskStore`] backed by a `HashMap`.
///
/// `create_subtask` panics — mech tasks are always leaves and never decompose.
/// Use [`MechStore::insert`] to add pre-built [`MechTask`]s, or
/// [`MechStore::create_root`] for the common single-task case.
pub struct MechStore {
    tasks: HashMap<TaskId, MechTask>,
    // Required by TaskStore trait; not read in leaf-only store.
    root_id: Option<TaskId>,
    next_id: u64,
    agent: Option<Arc<dyn AgentExecutor>>,
}

impl MechStore {
    /// Create an empty store with no agent and no tasks.
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            root_id: None,
            next_id: 0,
            agent: None,
        }
    }

    /// Insert a pre-built [`MechTask`] into the store and return its id.
    pub fn insert(&mut self, task: MechTask) -> TaskId {
        let id = task.id;
        self.tasks.insert(id, task);
        id
    }

    /// Create and insert the root task, recording it as `root_id`.
    pub fn create_root(
        &mut self,
        goal: impl Into<String>,
        workflow: Arc<Workflow>,
        function_name: impl Into<String>,
        input: JsonValue,
    ) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        let task = MechTask::new(id, goal, workflow, function_name, input, self.agent.clone());
        self.tasks.insert(id, task);
        self.root_id = Some(id);
        id
    }

    /// Builder method: set the agent that will be injected into tasks via
    /// [`cue::TaskStore::bind_runtime`].
    #[must_use]
    pub fn with_agent(mut self, agent: Arc<dyn AgentExecutor>) -> Self {
        self.agent = Some(agent);
        self
    }
}

impl Default for MechStore {
    fn default() -> Self {
        Self::new()
    }
}

impl cue::TaskStore for MechStore {
    type Task = MechTask;

    fn get(&self, id: TaskId) -> Option<&Self::Task> {
        self.tasks.get(&id)
    }
    fn get_mut(&mut self, id: TaskId) -> Option<&mut Self::Task> {
        self.tasks.get_mut(&id)
    }
    fn task_count(&self) -> usize {
        self.tasks.len()
    }

    fn dfs_order(&self, root: TaskId) -> Vec<TaskId> {
        if self.tasks.contains_key(&root) {
            vec![root]
        } else {
            vec![]
        }
    }

    fn set_root_id(&mut self, id: TaskId) {
        self.root_id = Some(id);
    }
    fn save(&self, _path: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    fn bind_runtime(&mut self) {
        if let Some(agent) = self.agent.clone() {
            for task in self.tasks.values_mut() {
                task.set_agent(Arc::clone(&agent));
            }
        }
    }

    fn create_subtask(
        &mut self,
        _parent_id: TaskId,
        _spec: &SubtaskSpec,
        _mark_fix: bool,
        _inherit_recovery_rounds: Option<u32>,
    ) -> TaskId {
        panic!("MechStore::create_subtask: mech tasks are always leaves");
    }

    fn any_non_fix_child_succeeded(&self, _parent_id: TaskId) -> bool {
        false
    }

    fn build_tree_context(&self, id: TaskId) -> Result<TreeContext, OrchestratorError> {
        if !self.tasks.contains_key(&id) {
            return Err(OrchestratorError::TaskNotFound(id));
        }
        Ok(TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: Vec::new(),
            ancestor_goals: Vec::new(),
            completed_siblings: Vec::new(),
            pending_sibling_goals: Vec::new(),
            children: Vec::new(),
            checkpoint_guidance: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MechError;
    use crate::exec::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
    use crate::loader::WorkflowLoader;
    use cue::{Orchestrator, TaskNode, TaskStore};
    use serde_json::json;
    use std::sync::Mutex;
    use traits::EventEmitter;

    fn run_blocking<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(fut)
    }

    fn load(yaml: &str) -> Arc<Workflow> {
        Arc::new(WorkflowLoader::new().load_str(yaml).expect("load"))
    }

    struct SeqAgent {
        responses: Mutex<Vec<JsonValue>>,
    }
    impl SeqAgent {
        fn new(responses: Vec<JsonValue>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }
    impl AgentExecutor for SeqAgent {
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

    struct RecordingAgent {
        models_seen: Arc<Mutex<Vec<Option<String>>>>,
        inner: SeqAgent,
    }
    impl RecordingAgent {
        fn new(responses: Vec<JsonValue>) -> (Self, Arc<Mutex<Vec<Option<String>>>>) {
            let log = Arc::new(Mutex::new(Vec::new()));
            let agent = Self {
                models_seen: Arc::clone(&log),
                inner: SeqAgent::new(responses),
            };
            (agent, log)
        }
    }
    impl AgentExecutor for RecordingAgent {
        fn run<'a>(
            &'a self,
            request: AgentRequest,
        ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
            self.models_seen.lock().unwrap().push(request.model.clone());
            self.inner.run(request)
        }
    }

    // EventLog stores events in a shared Arc so callers can read them after
    // Orchestrator (which takes ownership of the emitter) finishes.
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
    impl EventEmitter<cue::CueEvent> for EventLog {
        fn emit(&self, event: cue::CueEvent) {
            self.log.lock().unwrap().push(event);
        }
    }

    // S1 — raw string avoids escape noise.
    const SIMPLE_YAML: &str = r#"
functions:
  main:
    input: { type: object }
    blocks:
      greet:
        prompt: "hello"
        schema:
          type: object
          required: [greeting]
          properties: { greeting: { type: string } }
"#;

    const FULL_EXAMPLE: &str = include_str!("../testdata/full_example.yaml");

    #[test]
    fn mech_task_implements_task_node() {
        fn assert_task_node<T: cue::TaskNode>() {}
        assert_task_node::<MechTask>();
    }

    #[test]
    fn mech_store_implements_task_store() {
        fn assert_task_store<S: cue::TaskStore>() {}
        assert_task_store::<MechStore>();
    }

    #[test]
    fn orchestrator_completes_successfully() {
        let wf = load(SIMPLE_YAML);
        let agent: Arc<dyn AgentExecutor> =
            Arc::new(SeqAgent::new(vec![json!({ "greeting": "Hi!" })]));
        let mut store = MechStore::new().with_agent(agent);
        let root_id = store.create_root("run main", Arc::clone(&wf), "main", json!({}));
        let (emitter, _events) = EventLog::new();
        let mut orchestrator = Orchestrator::new(store, emitter);
        assert_eq!(
            run_blocking(orchestrator.run(root_id)).unwrap(),
            TaskOutcome::Success
        );
    }

    #[test]
    fn failed_workflow_maps_to_failed_outcome() {
        let yaml = r#"
functions:
  main:
    input: { type: object }
    blocks:
      step:
        prompt: "go"
        schema:
          type: object
          required: [result]
          properties: { result: { type: integer } }
"#;
        let wf = load(yaml);
        let agent: Arc<dyn AgentExecutor> =
            Arc::new(SeqAgent::new(vec![json!({ "result": "not-an-int" })]));
        let mut store = MechStore::new().with_agent(agent);
        let root_id = store.create_root("run main", Arc::clone(&wf), "main", json!({}));
        let (emitter, _events) = EventLog::new();
        let mut orchestrator = Orchestrator::new(store, emitter);
        let outcome = run_blocking(orchestrator.run(root_id)).unwrap();
        assert!(
            matches!(outcome, TaskOutcome::Failed { .. }),
            "expected Failed, got {outcome:?}"
        );
    }

    #[test]
    fn escalating_executor_overrides_model_when_no_block_override() {
        let models_log = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        struct LoggingAgent(Arc<Mutex<Vec<Option<String>>>>);
        impl AgentExecutor for LoggingAgent {
            fn run<'a>(
                &'a self,
                request: AgentRequest,
            ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
                self.0.lock().unwrap().push(request.model.clone());
                Box::pin(async move {
                    Ok(AgentResponse {
                        output: json!({ "greeting": "hi" }),
                        messages: vec![],
                    })
                })
            }
        }
        let inner = LoggingAgent(Arc::clone(&models_log));
        let escalating = EscalatingExecutor {
            inner: &inner,
            override_model: "opus".to_owned(),
            workflow_default_model: Some(Model::Haiku),
        };
        let schema = json!({ "type": "object", "required": ["greeting"], "properties": { "greeting": { "type": "string" } } });
        run_blocking(escalating.run(AgentRequest {
            model: None,
            system: None,
            prompt: "t".into(),
            grants: vec![],
            tools: vec![],
            write_paths: vec![],
            timeout: None,
            output_schema: schema.clone(),
            history: vec![],
        }))
        .unwrap();
        run_blocking(escalating.run(AgentRequest {
            model: Some("haiku".to_owned()),
            system: None,
            prompt: "t".into(),
            grants: vec![],
            tools: vec![],
            write_paths: vec![],
            timeout: None,
            output_schema: schema.clone(),
            history: vec![],
        }))
        .unwrap();
        run_blocking(escalating.run(AgentRequest {
            model: Some("my-block-model".to_owned()),
            system: None,
            prompt: "t".into(),
            grants: vec![],
            tools: vec![],
            write_paths: vec![],
            timeout: None,
            output_schema: schema,
            history: vec![],
        }))
        .unwrap();
        let log = models_log.lock().unwrap().clone();
        assert_eq!(log[0], Some("opus".to_owned()));
        assert_eq!(log[1], Some("opus".to_owned()));
        assert_eq!(log[2], Some("my-block-model".to_owned()));
    }

    #[test]
    fn execute_leaf_uses_escalated_model() {
        // Set assessment model to Opus to simulate cue escalation via set_assessment.
        // This exercises the path where set_assessment drives execute_leaf to use EscalatingExecutor.
        let yaml_with_default = r#"
workflow:
  agent:
    model: haiku
functions:
  main:
    input: { type: object }
    blocks:
      greet:
        prompt: "hello"
        schema:
          type: object
          required: [greeting]
          properties: { greeting: { type: string } }
"#;
        let wf = load(yaml_with_default);
        let (recording_agent, models_log) = RecordingAgent::new(vec![json!({ "greeting": "hi" })]);
        let agent_arc: Arc<dyn AgentExecutor> = Arc::new(recording_agent);
        let mut task = MechTask::new(
            TaskId(0),
            "run main",
            Arc::clone(&wf),
            "main",
            json!({}),
            Some(Arc::clone(&agent_arc)),
        );
        // Set assessment model to Opus to simulate cue escalation via set_assessment.
        task.set_assessment(TaskPath::Leaf, Model::Opus, None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        assert_eq!(run_blocking(task.execute_leaf(&ctx)), TaskOutcome::Success);
        assert_eq!(
            models_log.lock().unwrap().clone()[0],
            Some("opus".to_owned())
        );
    }

    #[test]
    fn model_escalation_through_set_assessment() {
        // Uses a workflow with a workflow-level agent model of "haiku".
        // set_assessment(Sonnet) should trigger EscalatingExecutor and the
        // agent should receive model: Some("sonnet").
        let yaml_with_default = r#"
workflow:
  agent:
    model: haiku
functions:
  main:
    input: { type: object }
    blocks:
      greet:
        prompt: "hello"
        schema:
          type: object
          required: [greeting]
          properties: { greeting: { type: string } }
"#;
        let wf = load(yaml_with_default);
        let (recording_agent, models_log) = RecordingAgent::new(vec![json!({ "greeting": "hi" })]);
        let agent_arc: Arc<dyn AgentExecutor> = Arc::new(recording_agent);
        let mut task = MechTask::new(
            TaskId(0),
            "run main",
            Arc::clone(&wf),
            "main",
            json!({}),
            Some(Arc::clone(&agent_arc)),
        );
        task.set_assessment(TaskPath::Leaf, Model::Sonnet, None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        assert_eq!(run_blocking(task.execute_leaf(&ctx)), TaskOutcome::Success);
        assert_eq!(
            models_log.lock().unwrap().clone()[0],
            Some("sonnet".to_owned())
        );
    }

    #[test]
    #[should_panic(expected = "agent not bound")]
    fn execute_leaf_panics_when_no_agent() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(
            TaskId(0),
            "run main",
            Arc::clone(&wf),
            "main",
            json!({}),
            None,
        );
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        run_blocking(task.execute_leaf(&ctx));
    }

    #[test]
    #[should_panic(expected = "mech tasks are always leaves")]
    fn verify_branch_panics() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        run_blocking(task.verify_branch(&ctx)).ok();
    }

    #[test]
    #[should_panic(expected = "mech tasks are always leaves")]
    fn design_fix_panics() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        run_blocking(task.design_fix(&ctx, "reason", 1, Model::Haiku)).ok();
    }

    #[test]
    #[should_panic(expected = "mech tasks are always leaves")]
    fn handle_checkpoint_panics() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        run_blocking(task.handle_checkpoint(&ctx, &[])).ok();
    }

    #[test]
    #[should_panic(expected = "mech tasks are always leaves")]
    fn assess_and_design_recovery_panics() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        run_blocking(task.assess_and_design_recovery(&ctx, "reason", 1)).ok();
    }

    #[test]
    #[should_panic(expected = "mech tasks are always leaves")]
    fn decompose_panics() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        let ctx = TreeContext {
            parent_goal: None,
            parent_decomposition_rationale: None,
            parent_discoveries: vec![],
            ancestor_goals: vec![],
            completed_siblings: vec![],
            pending_sibling_goals: vec![],
            children: vec![],
            checkpoint_guidance: None,
        };
        run_blocking(task.decompose(&ctx, Model::Haiku)).ok();
    }

    #[test]
    #[should_panic(expected = "mech tasks are always leaves")]
    fn create_subtask_panics() {
        let mut store = MechStore::new();
        let spec = cue::SubtaskSpec {
            goal: "test".into(),
            verification_criteria: vec![],
            magnitude_estimate: cue::MagnitudeEstimate::Small,
        };
        store.create_subtask(TaskId(0), &spec, false, None);
    }

    #[test]
    fn worked_example_via_orchestrator() {
        let wf = Arc::new(WorkflowLoader::new().load_str(FULL_EXAMPLE).expect("load"));
        let agent: Arc<dyn AgentExecutor> = Arc::new(SeqAgent::new(vec![
            json!({ "category": "other", "urgency": "low" }),
            json!({ "resolved": true, "summary": "Here is the info." }),
        ]));
        let mut store = MechStore::new().with_agent(agent);
        let root_id = store.create_root(
            "support triage",
            Arc::clone(&wf),
            "support_triage",
            json!({ "ticket_text": "How do I change my password?", "customer_tier": "free" }),
        );
        let (emitter, events) = EventLog::new();
        let mut orchestrator = Orchestrator::new(store, emitter);
        assert_eq!(
            run_blocking(orchestrator.run(root_id)).unwrap(),
            TaskOutcome::Success
        );
        assert!(events.lock().unwrap().iter().any(|e| matches!(
            e,
            cue::CueEvent::TaskCompleted {
                outcome: TaskOutcome::Success,
                ..
            }
        )));
    }

    #[test]
    fn forced_assessment_always_leaf() {
        let wf = load(SIMPLE_YAML);
        let task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        assert_eq!(task.forced_assessment(0).unwrap().path, TaskPath::Leaf);
        assert_eq!(task.forced_assessment(999).unwrap().path, TaskPath::Leaf);
    }

    #[test]
    fn resume_point_reflects_phase() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        task.phase = TaskPhase::Pending;
        assert_eq!(task.resume_point(), ResumePoint::NeedAssessment);
        task.phase = TaskPhase::Assessing;
        assert_eq!(task.resume_point(), ResumePoint::NeedAssessment);
        task.phase = TaskPhase::Executing;
        assert_eq!(task.resume_point(), ResumePoint::LeafExecuting);
        task.phase = TaskPhase::Verifying;
        assert_eq!(task.resume_point(), ResumePoint::LeafVerifying);
        task.phase = TaskPhase::Completed;
        assert_eq!(
            task.resume_point(),
            ResumePoint::Terminal(TaskOutcome::Success)
        );
        task.phase = TaskPhase::Failed;
        assert!(matches!(
            task.resume_point(),
            ResumePoint::Terminal(TaskOutcome::Failed { .. })
        ));
    }

    #[test]
    fn can_attempt_recovery_not_eligible() {
        let wf = load(SIMPLE_YAML);
        let task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        assert!(matches!(
            task.can_attempt_recovery(&LimitsConfig::default()),
            RecoveryEligibility::NotEligible { .. }
        ));
    }

    #[test]
    fn fix_round_budget_check_exhausted() {
        let wf = load(SIMPLE_YAML);
        let task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        assert!(matches!(
            task.fix_round_budget_check(&LimitsConfig::default()),
            FixBudgetCheck::Exhausted
        ));
    }

    #[test]
    fn needs_decomposition_false() {
        let wf = load(SIMPLE_YAML);
        let task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        assert!(!task.needs_decomposition());
    }

    #[test]
    fn completed_task_is_terminal_on_resume() {
        let wf = load(SIMPLE_YAML);
        let mut task = MechTask::new(TaskId(0), "run", wf, "main", json!({}), None);
        task.phase = TaskPhase::Completed;
        assert!(task.is_terminal());
        assert_eq!(
            task.resume_point(),
            ResumePoint::Terminal(TaskOutcome::Success)
        );
    }

    #[test]
    fn bind_runtime_injects_agent() {
        let wf = load(SIMPLE_YAML);
        let task = MechTask::new(TaskId(0), "run", Arc::clone(&wf), "main", json!({}), None);
        let mut store = MechStore::new();
        let id = store.insert(task);
        assert!(store.get(id).unwrap().agent.is_none());
        let agent: Arc<dyn AgentExecutor> =
            Arc::new(SeqAgent::new(vec![json!({ "greeting": "hi" })]));
        store.agent = Some(Arc::clone(&agent));
        store.bind_runtime();
        assert!(store.get(id).unwrap().agent.is_some());
    }
}
