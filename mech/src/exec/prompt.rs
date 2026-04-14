//! Prompt block executor (Deliverable 9, conversation scoping in Deliverable 13).
//!
//! Executes a single [`PromptBlock`]:
//!
//! 1. Resolve the effective agent configuration via the three-level cascade
//!    (workflow → function → block, replace semantics per spec §5.5.2).
//!    `$ref:#name` string form and the `extends:` field are resolved against
//!    the workflow-level `agents` map.
//! 2. Render the prompt template by evaluating CEL against the current
//!    [`ExecutionContext::namespaces`].
//! 3. Build an [`AgentRequest`] (model, system prompt, rendered user
//!    prompt, grant flags, custom tools, write paths, timeout, output
//!    schema, conversation history).
//! 4. Invoke the injected [`AgentExecutor`].
//! 5. Append returned messages (or synthesized user+assistant) to the
//!    function's conversation (§4.6). Check compaction threshold.
//! 6. Validate the returned JSON value against the block's declared output
//!    schema.
//! 7. Record the validated output under the block's ID in the execution
//!    context (write-once per invocation).
//!
//! Transitions, `set_context` / `set_workflow` side-effects, and block
//! scheduling are deliberately out of scope — they land in Deliverable 11
//! (transitions) and the later driver deliverables. Conversation
//! management (Deliverable 13) is integrated: each prompt block receives
//! and mutates a [`Conversation`] tracking the function’s message history.

use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::cel::{Namespaces, Template};
use crate::context::ExecutionContext;
use crate::conversation::Conversation;
use crate::error::{MechError, MechResult};
use crate::exec::agent::{AgentExecutor, AgentRequest};
use crate::loader::Workflow;
#[cfg(test)]
use crate::schema::BlockDef;
use crate::schema::resolve_nested_refs;
use crate::schema::{
    AgentConfig, AgentConfigRef, FunctionDef, PromptBlock, SchemaRef, WorkflowFile,
};

/// Resolved agent configuration: the three-level cascade with `extends:` and
/// `$ref:#name` fully expanded. Every field is optional because the agent
/// runtime supplies defaults for anything left unset.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedAgentConfig {
    /// Model name.
    pub model: Option<String>,
    /// ToolGrant flag strings.
    pub grant: Vec<String>,
    /// Custom tool names.
    pub tools: Vec<String>,
    /// Writable paths.
    pub write_paths: Vec<String>,
    /// Per-block timeout. Parsed from the raw `AgentConfig.timeout` string
    /// via [`parse_timeout`] at cascade-resolution time so the resolved form
    /// is ready to hand to the agent runtime without further processing.
    pub timeout: Option<Duration>,
}

impl ResolvedAgentConfig {
    fn from_inline(inline: &AgentConfig) -> MechResult<Self> {
        Ok(Self {
            model: inline.model.clone(),
            grant: inline.grant_list().to_vec(),
            tools: inline.tool_list().to_vec(),
            write_paths: inline.write_path_list().to_vec(),
            timeout: inline.timeout.as_deref().map(parse_timeout).transpose()?,
        })
    }
}

/// Resolve the effective agent configuration for a prompt block per the
/// three-level cascade in §5.5.2 ("replace semantics — each level fully
/// replaces the level above, with no field-level merging"). The only
/// intra-level merge is `extends:` which starts from a named workflow-level
/// config and overlays the specifying block's fields.
///
/// Returns `ResolvedAgentConfig::default()` when no level specified an
/// `agent:` field — the executor falls back to its runtime defaults.
pub fn resolve_agent_config(
    workflow: &WorkflowFile,
    function: &FunctionDef,
    block: &PromptBlock,
) -> MechResult<ResolvedAgentConfig> {
    // Lowest-priority first; highest-priority wins and *replaces*.
    let chosen = block
        .agent
        .as_ref()
        .or(function.agent.as_ref())
        .or_else(|| workflow.workflow.as_ref().and_then(|w| w.agent.as_ref()));

    let Some(chosen) = chosen else {
        return Ok(ResolvedAgentConfig::default());
    };

    let empty = Default::default();
    let agents = workflow
        .workflow
        .as_ref()
        .map(|w| &w.agents)
        .unwrap_or(&empty);

    resolve_agent_ref(chosen, agents)
}

/// Resolve a single [`AgentConfigRef`] against the workflow-level `agents`
/// map. Handles `$ref:#name`, inline configs, and inline configs with
/// `extends:`.
fn resolve_agent_ref(
    reference: &AgentConfigRef,
    agents: &std::collections::BTreeMap<String, AgentConfig>,
) -> MechResult<ResolvedAgentConfig> {
    match reference {
        AgentConfigRef::Ref(raw) => {
            let name = parse_hash_ref(raw)?;
            let base = agents.get(name).ok_or_else(|| MechError::Validation {
                errors: vec![format!(
                    "agent $ref:#{name} does not exist in workflow.agents"
                )],
            })?;
            // A bare $ref: the named config may itself use `extends:`.
            resolve_extends_chain(base, agents)
        }
        AgentConfigRef::Inline(inline) => {
            if inline.extends.is_some() {
                resolve_extends_chain(inline, agents)
            } else {
                ResolvedAgentConfig::from_inline(inline)
            }
        }
    }
}

/// Walk an `extends:` chain, applying overlay semantics at each level
/// (specified fields override the base; unspecified fields inherit). Cycle
/// detection is a `debug_assert!` — the load-time validator rejects cycles
/// (§10.1); this catches regressions in debug builds without paying the cost
/// at runtime.
fn resolve_extends_chain(
    inline: &AgentConfig,
    agents: &std::collections::BTreeMap<String, AgentConfig>,
) -> MechResult<ResolvedAgentConfig> {
    let mut chain: Vec<&AgentConfig> = vec![inline];
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut cursor = inline;
    // Hard bound: a valid chain can visit each named agent at most once, so
    // `agents.len() + 1` (plus the inline root) is an upper bound. In release
    // builds this prevents a runaway loop if the loader invariant is ever
    // violated; in debug builds the `debug_assert!` below fires first.
    let max_depth = agents.len() + 2;
    while let Some(parent_name) = &cursor.extends {
        debug_assert!(
            seen.insert(parent_name.clone()),
            "loader invariant: extends cycle at `{parent_name}` should have been rejected at load time"
        );
        if chain.len() > max_depth {
            return Err(MechError::Validation {
                errors: vec![format!(
                    "agent extends chain exceeded bound at `{parent_name}` (cycle?)"
                )],
            });
        }
        let parent = agents
            .get(parent_name)
            .ok_or_else(|| MechError::Validation {
                errors: vec![format!("agent extends target `{parent_name}` not found")],
            })?;
        chain.push(parent);
        cursor = parent;
    }
    // Fold from the deepest ancestor up to the inline override: later
    // entries in `chain` are bases, earlier entries are overrides.
    let mut resolved = ResolvedAgentConfig::default();
    for level in chain.iter().rev() {
        overlay(&mut resolved, level)?;
    }
    Ok(resolved)
}

fn overlay(into: &mut ResolvedAgentConfig, from: &AgentConfig) -> MechResult<()> {
    if from.model.is_some() {
        into.model = from.model.clone();
    }
    if let Some(grant) = &from.grant {
        into.grant = grant.clone();
    }
    if let Some(tools) = &from.tools {
        into.tools = tools.clone();
    }
    if let Some(write_paths) = &from.write_paths {
        into.write_paths = write_paths.clone();
    }
    if let Some(raw) = from.timeout.as_deref() {
        into.timeout = Some(parse_timeout(raw)?);
    }
    Ok(())
}

fn parse_hash_ref(raw: &str) -> MechResult<&str> {
    let body = raw
        .strip_prefix("$ref:")
        .ok_or_else(|| MechError::Validation {
            errors: vec![format!("malformed agent $ref: `{raw}`")],
        })?;
    body.strip_prefix('#')
        .filter(|s| !s.is_empty())
        .ok_or_else(|| MechError::Validation {
            errors: vec![format!("malformed agent $ref: `{raw}`")],
        })
}

/// Parse a timeout string like `"30s"`, `"5m"`, `"250ms"`.
fn parse_timeout(s: &str) -> MechResult<Duration> {
    let s = s.trim();
    let (num_str, unit) = if let Some(n) = s.strip_suffix("ms") {
        (n, "ms")
    } else if let Some(n) = s.strip_suffix('s') {
        (n, "s")
    } else if let Some(n) = s.strip_suffix('m') {
        (n, "m")
    } else if let Some(n) = s.strip_suffix('h') {
        (n, "h")
    } else {
        return Err(MechError::Validation {
            errors: vec![format!("invalid timeout `{s}`: missing unit suffix")],
        });
    };
    let n: u64 = num_str.parse().map_err(|_| MechError::Validation {
        errors: vec![format!("invalid timeout `{s}`: bad number")],
    })?;
    if n == 0 {
        return Err(MechError::Validation {
            errors: vec![format!("invalid timeout `{s}`: timeout must be > 0")],
        });
    }
    Ok(match unit {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 3600),
        _ => unreachable!(),
    })
}

/// Extract the inline JSON Schema for a prompt block's output.
///
/// Inline schemas return their body directly; `$ref:#name` schemas are
/// looked up in the workflow-level shared schemas map.  Nested `$ref:#name`
/// references within the looked-up schema body are recursively resolved
/// via [`resolve_nested_refs`].  `output: infer` is forbidden at the block
/// level — by the time the loader is done, every
/// prompt block has a concrete schema.
fn resolve_prompt_block_schema(
    workflow: &WorkflowFile,
    block: &PromptBlock,
) -> MechResult<JsonValue> {
    match &block.schema {
        SchemaRef::Inline(v) => Ok(v.clone()),
        SchemaRef::Ref(raw) => {
            let name = parse_hash_ref(raw)?;
            let schemas_map = workflow
                .workflow
                .as_ref()
                .map(|w| &w.schemas)
                .ok_or_else(|| MechError::SchemaRefUnresolved {
                    name: name.to_string(),
                })?;
            let shared = schemas_map
                .get(name)
                .ok_or_else(|| MechError::SchemaRefUnresolved {
                    name: name.to_string(),
                })?;
            resolve_nested_refs(shared, schemas_map)
        }
        SchemaRef::Infer(_) => Err(MechError::Validation {
            errors: vec!["prompt block schema cannot be `infer`".into()],
        }),
    }
}

/// Fetch an interned compiled template from the loader cache and render it
/// against the supplied namespaces. The loader guarantees every workflow
/// template string is interned at load time, so a cache miss is an internal
/// invariant violation.
fn render_template(
    workflow: &Workflow,
    source: &str,
    namespaces: &Namespaces,
) -> MechResult<String> {
    let tmpl: &Template =
        workflow
            .template(source)
            .ok_or_else(|| MechError::InternalInvariant {
                message: format!("template `{source}` should have been interned at load time"),
            })?;
    tmpl.render(namespaces)
}

/// Attach a block ID to any [`MechError`] that carries a per-block `block`
/// field, so executor errors surfaced out of `AgentExecutor::run` are tagged
/// with the block they originated from. Variants that do not carry a block
/// context are returned unchanged.
fn tag_executor_error(err: MechError, block_id: &str) -> MechError {
    match err {
        MechError::LlmCallFailure { message, .. } => MechError::LlmCallFailure {
            block: block_id.to_string(),
            message,
        },
        MechError::SchemaValidationFailure {
            details,
            raw_output,
            ..
        } => MechError::SchemaValidationFailure {
            block: block_id.to_string(),
            details,
            raw_output,
        },
        MechError::GuardEvaluationError {
            expression,
            message,
            ..
        } => MechError::GuardEvaluationError {
            block: block_id.to_string(),
            expression,
            message,
        },
        MechError::TemplateResolutionError {
            expression,
            message,
            ..
        } => MechError::TemplateResolutionError {
            block: block_id.to_string(),
            expression,
            message,
        },
        MechError::Timeout { duration, .. } => MechError::Timeout {
            block: block_id.to_string(),
            duration,
        },
        other => other,
    }
}

/// Execute a single prompt block.
///
/// Rendering, dispatch, validation, and block-output storage all happen
/// here. Transitions and `set_context` / `set_workflow` writes are a
/// separate concern (Deliverable 11).
///
/// The `conversation` parameter carries the function's accumulated
/// conversation history (§4.6). The prompt/response and any tool
/// call/result messages from the agent loop are appended after each
/// execution.
#[allow(clippy::too_many_arguments)]
pub async fn execute_prompt_block(
    workflow: &Workflow,
    function: &FunctionDef,
    block_id: &str,
    block: &PromptBlock,
    ctx: &mut ExecutionContext,
    executor: &dyn AgentExecutor,
    conversation: &mut Conversation,
) -> MechResult<JsonValue> {
    let file = workflow.file();

    // 1. Agent cascade.
    let resolved_agent = resolve_agent_config(file, function, block)?;

    // 2. Render the prompt template. The loader interns every template at
    //    load time, so a cache miss is an internal invariant violation.
    let namespaces = ctx.namespaces();
    let rendered_prompt = render_template(workflow, &block.prompt, &namespaces)?;

    // 3. Render the system prompt (function override beats workflow default).
    let system_source = function
        .system
        .as_deref()
        .or_else(|| file.workflow.as_ref().and_then(|w| w.system.as_deref()));
    let rendered_system = match system_source {
        Some(src) => Some(render_template(workflow, src, &namespaces)?),
        None => None,
    };

    // 4. Resolve the output schema to JSON for the agent request.
    let output_schema = resolve_prompt_block_schema(file, block)?;

    // 5. Build the request with conversation history.
    let request = AgentRequest {
        model: resolved_agent.model,
        system: rendered_system,
        prompt: rendered_prompt.clone(),
        grant: resolved_agent.grant,
        tools: resolved_agent.tools,
        write_paths: resolved_agent.write_paths,
        timeout: resolved_agent.timeout,
        output_schema: output_schema.clone(),
        history: conversation.messages().to_vec(),
    };

    // 6. Dispatch.
    let response = executor
        .run(request)
        .await
        .map_err(|e| tag_executor_error(e, block_id))?;
    let output = response.output;

    // 6b. Append messages to conversation. If the agent returned
    //     messages, use those (they may include tool call/result pairs).
    //     Otherwise, synthesize minimal user+assistant messages.
    if response.messages.is_empty() {
        conversation.push(crate::conversation::Message::user(rendered_prompt));
        conversation.push(crate::conversation::Message::assistant(output.to_string()));
    } else {
        conversation.push_many(response.messages);
    }

    // 6c. Check if compaction should fire.
    conversation.check_compaction();

    // 7. Validate against the declared output schema. Surface up to the
    //    first 10 errors so authors see more than the single first failure.
    let validator =
        jsonschema::validator_for(&output_schema).map_err(|e| MechError::SchemaInvalid {
            name: format!("<block {block_id}>"),
            message: e.to_string(),
        })?;
    let errors: Vec<String> = validator
        .iter_errors(&output)
        .take(10)
        .map(|err| {
            format!(
                "{err} (instance=`{}`, schema=`{}`)",
                err.instance_path, err.schema_path
            )
        })
        .collect();
    if !errors.is_empty() {
        return Err(MechError::SchemaValidationFailure {
            block: block_id.to_string(),
            details: errors.join("; "),
            raw_output: output.to_string(),
        });
    }

    // 8. Record under `block.<id>`.
    ctx.record_block_output(block_id, output.clone())?;
    Ok(output)
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, WorkflowState};
    use crate::conversation::Conversation;
    use crate::exec::agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
    use crate::loader::WorkflowLoader;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Test-only agent executor. Captures the last request it saw and
    /// returns a fixed response. A closure variant lets tests compute the
    /// response from the request (needed for schema-mismatch tests).
    type HandlerFn = dyn Fn(&AgentRequest) -> Result<AgentResponse, MechError> + Send + Sync;

    struct FakeAgent {
        handler: Box<HandlerFn>,
        last: Mutex<Option<AgentRequest>>,
    }

    impl FakeAgent {
        fn new(
            handler: impl Fn(&AgentRequest) -> Result<AgentResponse, MechError> + Send + Sync + 'static,
        ) -> Self {
            Self {
                handler: Box::new(handler),
                last: Mutex::new(None),
            }
        }

        fn fixed(output: JsonValue) -> Self {
            Self::new(move |_| {
                Ok(AgentResponse {
                    output: output.clone(),
                    messages: vec![],
                })
            })
        }

        fn last(&self) -> AgentRequest {
            self.last
                .lock()
                .unwrap()
                .clone()
                .expect("no request captured")
        }
    }

    impl AgentExecutor for FakeAgent {
        fn run<'a>(
            &'a self,
            request: AgentRequest,
        ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
            let res = (self.handler)(&request);
            *self.last.lock().unwrap() = Some(request);
            Box::pin(async move { res })
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

    fn new_ctx(decls: &BTreeMap<String, crate::schema::ContextVarDef>) -> ExecutionContext {
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        ExecutionContext::new(
            json!({ "user": "ada" }),
            json!({ "run_id": "r1" }),
            decls,
            ws,
        )
        .unwrap()
    }

    const TRIVIAL: &str = r#"
functions:
  f:
    input: { type: object }
    blocks:
      classify:
        prompt: "hi {{input.user}}"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
"#;

    #[test]
    fn trivial_prompt_block_stores_output_in_context() {
        let wf = load(TRIVIAL);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt"),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "category": "billing" }));

        let out = run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .expect("execute");

        assert_eq!(out, json!({ "category": "billing" }));
        assert_eq!(
            ctx.get_block_output("classify").unwrap(),
            &json!({ "category": "billing" })
        );
    }

    #[test]
    fn prompt_template_interpolation_uses_current_context() {
        let wf = load(TRIVIAL);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt"),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "category": "x" }));

        run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .unwrap();

        assert_eq!(agent.last().prompt, "hi ada");
    }

    #[test]
    fn output_schema_mismatch_is_runtime_error() {
        let wf = load(TRIVIAL);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt"),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        // `category` missing — fails `required`.
        let agent = FakeAgent::fixed(json!({ "wrong": 1 }));

        let err = run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .expect_err("schema mismatch must error");
        match err {
            MechError::SchemaValidationFailure {
                block: b,
                raw_output,
                ..
            } => {
                assert_eq!(b, "classify");
                assert!(raw_output.contains("wrong"));
            }
            other => panic!("expected SchemaValidationFailure, got {other:?}"),
        }
        // Output must NOT have been recorded in context.
        assert!(ctx.get_block_output("classify").is_err());
    }

    const CASCADE: &str = r#"
workflow:
  agents:
    base:
      model: haiku
      grant: [tools]
      write_paths: [base_path/]
  agent:
    model: sonnet
    grant: [network]
functions:
  wins_function:
    input: { type: object }
    agent:
      model: opus
      grant: [tools]
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
  wins_block:
    input: { type: object }
    agent:
      model: opus
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        agent:
          model: claude-3-5
          grant: [write]
          write_paths: [out/]
  uses_extends:
    input: { type: object }
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        agent:
          extends: base
          model: opus
  uses_ref:
    input: { type: object }
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        agent: "$ref:#base"
  uses_workflow_default:
    input: { type: object }
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
"#;

    fn get_resolved(fn_name: &str) -> ResolvedAgentConfig {
        let wf = load(CASCADE);
        let func = wf.file().functions.get(fn_name).unwrap();
        let block = match &func.blocks["b"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("prompt"),
        };
        resolve_agent_config(wf.file(), func, &block).unwrap()
    }

    #[test]
    fn cascade_function_replaces_workflow_default() {
        let r = get_resolved("wins_function");
        assert_eq!(r.model.as_deref(), Some("opus"));
        assert_eq!(r.grant, vec!["tools".to_string()]);
        // `network` from workflow default must NOT leak in (replace semantics).
        assert!(!r.grant.contains(&"network".to_string()));
    }

    #[test]
    fn cascade_block_replaces_function_and_workflow() {
        let r = get_resolved("wins_block");
        assert_eq!(r.model.as_deref(), Some("claude-3-5"));
        assert_eq!(r.grant, vec!["write".to_string()]);
        assert_eq!(r.write_paths, vec!["out/".to_string()]);
    }

    #[test]
    fn cascade_workflow_default_used_when_no_override() {
        let r = get_resolved("uses_workflow_default");
        assert_eq!(r.model.as_deref(), Some("sonnet"));
        assert_eq!(r.grant, vec!["network".to_string()]);
    }

    #[test]
    fn cascade_extends_overlays_named_config() {
        let r = get_resolved("uses_extends");
        // model from inline override, grant + write_paths inherited from base.
        assert_eq!(r.model.as_deref(), Some("opus"));
        assert_eq!(r.grant, vec!["tools".to_string()]);
        assert_eq!(r.write_paths, vec!["base_path/".to_string()]);
    }

    #[test]
    fn cascade_ref_resolves_named_config() {
        let r = get_resolved("uses_ref");
        assert_eq!(r.model.as_deref(), Some("haiku"));
        assert_eq!(r.grant, vec!["tools".to_string()]);
        assert_eq!(r.write_paths, vec!["base_path/".to_string()]);
    }

    #[test]
    fn extends_empty_grant_clears_parent() {
        let yaml = r#"
workflow:
  agents:
    base:
      model: sonnet
      grant: [tools, write]
      write_paths: [src/]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        agent:
          extends: base
          grant: []
          tools: []
          write_paths: []
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["b"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt block"),
        };
        let r = resolve_agent_config(wf.file(), func, &block).unwrap();
        assert!(
            r.grant.is_empty(),
            "grant: [] should clear inherited grants"
        );
        assert!(r.tools.is_empty(), "tools: [] should clear inherited tools");
        assert!(
            r.write_paths.is_empty(),
            "write_paths: [] should clear inherited write_paths"
        );
    }

    #[test]
    fn cascade_block_empty_grant_replaces_function_level() {
        let yaml = r#"
workflow:
  agent:
    model: sonnet
    grant: [network]
functions:
  f:
    input: { type: object }
    agent:
      grant: [tools, write]
      write_paths: [src/]
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        agent:
          model: haiku
          grant: []
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["b"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt block"),
        };
        let r = resolve_agent_config(wf.file(), func, &block).unwrap();
        // Block-level replaces function-level entirely; workflow default does not leak.
        assert_eq!(r.model.as_deref(), Some("haiku"), "block model wins");
        assert!(
            r.grant.is_empty(),
            "grant: [] at block level must clear, not inherit function grants"
        );
        // write_paths was only on function level; block replaces entirely so it must be empty.
        assert!(
            r.write_paths.is_empty(),
            "write_paths from function level must not leak into block"
        );
    }

    #[test]
    fn extends_omitted_fields_inherit_parent() {
        let yaml = r#"
workflow:
  agents:
    base:
      model: sonnet
      grant: [tools, write]
      tools: [web_search]
      write_paths: [src/]
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "p"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        agent:
          extends: base
          model: opus
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["b"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt block"),
        };
        let r = resolve_agent_config(wf.file(), func, &block).unwrap();
        assert_eq!(r.model.as_deref(), Some("opus"), "model overridden");
        assert_eq!(
            r.grant,
            vec!["tools", "write"],
            "grant inherited from parent"
        );
        assert_eq!(r.tools, vec!["web_search"], "tools inherited from parent");
        assert_eq!(
            r.write_paths,
            vec!["src/"],
            "write_paths inherited from parent"
        );
    }

    #[test]
    fn tool_grants_and_write_paths_are_passed_through_to_request() {
        let yaml = r#"
workflow:
  agents:
    w:
      model: sonnet
      grant: [write, network]
      tools: [web_search]
      write_paths: [src/, docs/]
      timeout: 45s
functions:
  f:
    input: { type: object }
    blocks:
      classify:
        prompt: "hi"
        schema:
          type: object
          required: [category]
          properties:
            category: { type: string }
        agent: "$ref:#w"
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "category": "x" }));
        run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .unwrap();

        let req = agent.last();
        assert_eq!(req.model.as_deref(), Some("sonnet"));
        assert_eq!(req.grant, vec!["write".to_string(), "network".to_string()]);
        assert_eq!(req.tools, vec!["web_search".to_string()]);
        assert_eq!(
            req.write_paths,
            vec!["src/".to_string(), "docs/".to_string()]
        );
        assert_eq!(req.timeout, Some(Duration::from_secs(45)));
        assert_eq!(
            req.output_schema.get("required").unwrap(),
            &json!(["category"])
        );
    }

    #[test]
    fn parse_timeout_accepts_all_units() {
        assert_eq!(parse_timeout("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_timeout("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_timeout("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_timeout("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_timeout("nope").is_err());
    }

    #[test]
    fn parse_timeout_rejects_zero() {
        for input in ["0s", "0ms", "0m", "0h"] {
            let err = parse_timeout(input).expect_err(input);
            let msg = format!("{err}");
            assert!(
                msg.contains("> 0"),
                "expected `> 0` in error for `{input}`, got `{msg}`"
            );
        }
    }

    // ---- F6/T1: system prompt rendering -----------------------------------

    #[test]
    fn system_prompt_is_rendered_against_context() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    system: "helping {{input.user}}"
    blocks:
      classify:
        prompt: "go"
        schema:
          type: object
          required: [category]
          properties: { category: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "category": "x" }));
        run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .unwrap();
        assert_eq!(agent.last().system.as_deref(), Some("helping ada"));
    }

    // ---- F6/T2: LlmCallFailure block tagging ------------------------------

    #[test]
    fn llm_call_failure_is_tagged_with_block_id() {
        let wf = load(TRIVIAL);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        // Use a non-empty sentinel to prove the tag unconditionally overwrites
        // whatever block id the executor returned, rather than only filling in
        // empty strings.
        let agent = FakeAgent::new(|_| {
            Err(MechError::LlmCallFailure {
                block: "wrong_block".into(),
                message: "boom".into(),
            })
        });
        let err = run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .expect_err("llm failure");
        match err {
            MechError::LlmCallFailure { block, message } => {
                assert_eq!(block, "classify");
                assert_eq!(message, "boom");
            }
            other => panic!("expected LlmCallFailure, got {other:?}"),
        }
    }

    // ---- F6/T3: $ref:#name schema resolution -------------------------------

    const SHARED_SCHEMA_YAML: &str = r#"
workflow:
  schemas:
    Category:
      type: object
      required: [category]
      properties:
        category: { type: string }
functions:
  f:
    input: { type: object }
    blocks:
      classify:
        prompt: "hi"
        schema: "$ref:#Category"
"#;

    #[test]
    fn shared_schema_ref_validates_conforming_output() {
        let wf = load(SHARED_SCHEMA_YAML);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "category": "ok" }));
        let out = run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .unwrap();
        assert_eq!(out, json!({ "category": "ok" }));
    }

    #[test]
    fn shared_schema_ref_rejects_nonconforming_output() {
        let wf = load(SHARED_SCHEMA_YAML);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "wrong": 1 }));
        let err = run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .expect_err("bad schema");
        assert!(matches!(err, MechError::SchemaValidationFailure { .. }));
    }

    // ---- F6/T4: agent cascade error paths ---------------------------------

    #[test]
    fn agent_ref_unknown_name_errors() {
        // Drive the private resolver directly — the loader rejects these
        // shapes at load time, so we can't exercise them via YAML.
        fn msg(err: MechError) -> String {
            match err {
                MechError::Validation { errors } => errors.join(" | "),
                other => panic!("expected Validation, got {other:?}"),
            }
        }
        let agents = std::collections::BTreeMap::new();
        // (a) $ref:#unknown — agent name not in map
        let err = resolve_agent_ref(&AgentConfigRef::Ref("$ref:#nope".into()), &agents)
            .expect_err("unknown ref");
        let m = msg(err);
        assert!(
            m.contains("nope") && m.contains("does not exist"),
            "case a: {m}"
        );
        // (b) inline extends: unknown
        let inline = AgentConfig {
            extends: Some("ghost".into()),
            ..AgentConfig::default()
        };
        let err = resolve_agent_ref(&AgentConfigRef::Inline(inline), &agents)
            .expect_err("unknown extends");
        let m = msg(err);
        assert!(
            m.contains("ghost") && m.contains("extends target"),
            "case b: {m}"
        );
        // (c) malformed $ref: syntax (no hash)
        let err = resolve_agent_ref(&AgentConfigRef::Ref("$ref:noHash".into()), &agents)
            .expect_err("malformed");
        let m = msg(err);
        assert!(m.contains("malformed agent $ref"), "case c: {m}");
    }

    // ---- F6/T5: multi-namespace template ----------------------------------

    #[test]
    fn rendered_prompt_can_read_all_namespaces() {
        let yaml = r#"
workflow:
  context:
    counter: { type: integer, initial: 7 }
functions:
  f:
    input: { type: object }
    output:
      type: object
      required: [ok]
      properties: { ok: { type: boolean } }
    context:
      note: { type: string, initial: "hello" }
    blocks:
      first:
        prompt: "first"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
      second:
        depends_on: [first]
        prompt: "u={{input.user}} n={{context.note}} w={{workflow.counter}} b={{block.first.output.val}}"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let first_block = match &func.blocks["first"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let second_block = match &func.blocks["second"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        // Build context with the function's declared context vars.
        let fn_decls = func.context.clone();
        let ws = WorkflowState::from_declarations(
            &wf.file()
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

        let first_agent = FakeAgent::fixed(json!({ "val": "FIRST" }));
        run_blocking(execute_prompt_block(
            &wf,
            func,
            "first",
            &first_block,
            &mut ctx,
            &first_agent,
            &mut Conversation::new(),
        ))
        .unwrap();

        let second_agent = FakeAgent::fixed(json!({ "ok": true }));
        run_blocking(execute_prompt_block(
            &wf,
            func,
            "second",
            &second_block,
            &mut ctx,
            &second_agent,
            &mut Conversation::new(),
        ))
        .unwrap();
        let rendered = second_agent.last().prompt;
        assert!(rendered.contains("u=ada"), "got: {rendered}");
        assert!(rendered.contains("n=hello"), "got: {rendered}");
        assert!(rendered.contains("w=7"), "got: {rendered}");
        assert!(rendered.contains("b=FIRST"), "got: {rendered}");
    }

    // ---- F6/T6: default request when no agent configured ------------------

    #[test]
    fn default_request_when_no_agent_configured() {
        let wf = load(TRIVIAL);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["classify"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!(),
        };
        let mut ctx = new_ctx(&BTreeMap::new());
        let agent = FakeAgent::fixed(json!({ "category": "x" }));
        run_blocking(execute_prompt_block(
            &wf,
            func,
            "classify",
            &block,
            &mut ctx,
            &agent,
            &mut Conversation::new(),
        ))
        .unwrap();
        let req = agent.last();
        assert_eq!(req.model, None);
        assert!(req.grant.is_empty());
        assert!(req.tools.is_empty());
        assert!(req.write_paths.is_empty());
        assert_eq!(req.timeout, None);
    }

    // ---- Issue #62: nested $ref:#name in shared schemas -------------------

    #[test]
    fn shared_schema_with_nested_ref_validates_correctly() {
        let yaml = r#"
workflow:
  schemas:
    Inner:
      type: object
      required: [value]
      properties:
        value: { type: integer, minimum: 1 }
    Outer:
      type: object
      required: [inner]
      properties:
        inner:
          $ref: '#Inner'
functions:
  f:
    input: { type: object }
    blocks:
      b:
        prompt: "go"
        schema: "$ref:#Outer"
"#;
        let wf = load(yaml);
        let func = wf.file().functions.get("f").unwrap();
        let block = match &func.blocks["b"] {
            BlockDef::Prompt(p) => p.clone(),
            _ => panic!("expected prompt"),
        };

        // Conforming output passes.
        {
            let mut ctx = new_ctx(&BTreeMap::new());
            let agent = FakeAgent::fixed(json!({ "inner": { "value": 5 } }));
            let out = run_blocking(execute_prompt_block(
                &wf,
                func,
                "b",
                &block,
                &mut ctx,
                &agent,
                &mut Conversation::new(),
            ))
            .expect("conforming output must pass");
            assert_eq!(out, json!({ "inner": { "value": 5 } }));
        }

        // Non-conforming output (value below minimum) fails.
        {
            let mut ctx = new_ctx(&BTreeMap::new());
            let agent = FakeAgent::fixed(json!({ "inner": { "value": 0 } }));
            let err = run_blocking(execute_prompt_block(
                &wf,
                func,
                "b",
                &block,
                &mut ctx,
                &agent,
                &mut Conversation::new(),
            ))
            .expect_err("non-conforming output must fail");
            assert!(
                matches!(err, MechError::SchemaValidationFailure { .. }),
                "expected SchemaValidationFailure, got {err:?}"
            );
        }
    }
}
