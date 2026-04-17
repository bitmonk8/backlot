//! Dataflow scheduler (Deliverable 12).
//!
//! Implements pure-dataflow execution for functions whose blocks are connected
//! only by `depends_on` edges (no transitions). Algorithm per spec §4.3:
//!
//! 1. Find terminal blocks (no outgoing transitions, not depended upon).
//! 2. Backward walk from terminals along `depends_on` edges → reachable set.
//! 3. Topological sort reachable blocks into execution levels by depth.
//! 4. Execute level-by-level (sequential within each level — parallel is
//!    future work).
//! 5. Collect terminal outputs into a map (multiple sinks) or return the
//!    single terminal's output.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;

use crate::context::ExecutionContext;
use crate::conversation::Conversation;
use crate::error::{MechError, MechResult};
use crate::exec::agent::AgentExecutor;
use crate::exec::call::{FunctionExecutor, execute_call_block};
use crate::exec::prompt::execute_prompt_block;
use crate::schema::{BlockDef, FunctionDef};
use crate::workflow::Workflow;

/// Extract `depends_on` list from a block.
fn block_depends_on(block: &BlockDef) -> &[String] {
    match block {
        BlockDef::Prompt(p) => &p.depends_on,
        BlockDef::Call(c) => &c.depends_on,
    }
}

/// Find terminal blocks for dataflow execution.
///
/// If the function declares explicit `terminals`, use those. Otherwise,
/// terminals are blocks with no outgoing transitions that are not referenced
/// in any other block's `depends_on`.
pub fn find_dataflow_terminals(function: &FunctionDef) -> MechResult<Vec<String>> {
    if !function.terminals.is_empty() {
        return Ok(function.terminals.clone());
    }

    // Collect all blocks referenced as dependencies.
    let mut depended_upon: BTreeSet<&str> = BTreeSet::new();
    for block in function.blocks.values() {
        for dep in block_depends_on(block) {
            depended_upon.insert(dep);
        }
    }

    let mut terminals: Vec<String> = Vec::new();
    for (name, block) in &function.blocks {
        let has_transitions = match block {
            BlockDef::Prompt(p) => !p.transitions.is_empty(),
            BlockDef::Call(c) => !c.transitions.is_empty(),
        };
        if !has_transitions && !depended_upon.contains(name.as_str()) {
            terminals.push(name.clone());
        }
    }

    if terminals.is_empty() {
        return Err(MechError::WorkflowValidation {
            errors: vec!["dataflow mode: no terminal blocks found".into()],
        });
    }

    Ok(terminals)
}

/// Walk `depends_on` edges backward from `seeds` to find all reachable blocks.
pub fn backward_reachable(
    blocks: &BTreeMap<String, BlockDef>,
    seeds: &[String],
) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    let mut stack: Vec<String> = seeds.to_vec();

    while let Some(name) = stack.pop() {
        if !reachable.insert(name.clone()) {
            continue; // Already visited.
        }
        if let Some(block) = blocks.get(&name) {
            for dep in block_depends_on(block) {
                if !reachable.contains(dep) {
                    stack.push(dep.clone());
                }
            }
        }
    }

    reachable
}

/// Topologically sort `reachable` blocks into execution levels by dependency
/// depth. Level 0 contains blocks with no dependencies (roots); level N
/// contains blocks whose deepest dependency is in level N-1.
///
/// Returns levels in execution order (roots first).
pub fn topo_sort_levels(
    blocks: &BTreeMap<String, BlockDef>,
    reachable: &BTreeSet<String>,
) -> MechResult<Vec<Vec<String>>> {
    // Build in-degree map (count of depends_on edges within the reachable set).
    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

    for name in reachable {
        in_degree.entry(name.as_str()).or_insert(0);
        if let Some(block) = blocks.get(name) {
            for dep in block_depends_on(block) {
                if reachable.contains(dep) {
                    *in_degree.entry(name.as_str()).or_insert(0) += 1;
                    dependents
                        .entry(dep.as_str())
                        .or_default()
                        .push(name.as_str());
                }
            }
        }
    }

    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut remaining = in_degree.clone();

    loop {
        // Collect all nodes with in-degree 0.
        let ready: Vec<String> = remaining
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(&name, _)| name.to_string())
            .collect();

        if ready.is_empty() {
            if remaining.is_empty() {
                break;
            }
            // Cycle detected — should have been caught at load time.
            return Err(MechError::WorkflowValidation {
                errors: vec![format!(
                    "dataflow cycle detected among: {:?}",
                    remaining.keys().collect::<Vec<_>>()
                )],
            });
        }

        // Remove ready nodes and decrement dependents' in-degrees.
        for name in &ready {
            remaining.remove(name.as_str());
            if let Some(deps) = dependents.get(name.as_str()) {
                for dep in deps {
                    if let Some(deg) = remaining.get_mut(*dep) {
                        *deg = deg.saturating_sub(1);
                    }
                }
            }
        }

        levels.push(ready);
    }

    Ok(levels)
}

/// Execute a single block (prompt or call) and record its output.
///
/// Dataflow blocks are single-turn per §4.6 rule 3: each gets a fresh
/// conversation with no shared history.
async fn execute_block(
    workflow: &Workflow,
    function: &FunctionDef,
    block_id: &str,
    ctx: &mut ExecutionContext,
    agent_executor: &dyn AgentExecutor,
    func_executor: &dyn FunctionExecutor,
) -> MechResult<JsonValue> {
    let block = function
        .blocks
        .get(block_id)
        .ok_or_else(|| MechError::WorkflowValidation {
            errors: vec![format!("dataflow: block `{block_id}` not found")],
        })?;

    let output = match block {
        BlockDef::Prompt(p) => {
            // Each dataflow prompt block gets a fresh single-turn
            // conversation (§4.6 rule 3: data edges do not carry history).
            let mut conversation = Conversation::new();
            execute_prompt_block(
                workflow,
                function,
                block_id,
                p,
                ctx,
                agent_executor,
                &mut conversation,
            )
            .await?
        }
        BlockDef::Call(c) => {
            execute_call_block(workflow, function, block_id, c, ctx, func_executor).await?
        }
    };

    // Apply side effects (set_context, set_workflow) — same as imperative.
    let (set_context, set_workflow) = match block {
        BlockDef::Prompt(p) => (&p.set_context, &p.set_workflow),
        BlockDef::Call(c) => (&c.set_context, &c.set_workflow),
    };
    crate::exec::schedule::apply_side_effects(
        workflow,
        block_id,
        set_context,
        set_workflow,
        &output,
        ctx,
    )?;

    Ok(output)
}

/// Run a function in dataflow mode.
///
/// Finds terminals, walks backward to find reachable blocks, topo-sorts into
/// levels, executes level-by-level, and collects terminal output.
pub async fn run_function_dataflow(
    workflow: &Workflow,
    _function_name: &str,
    function: &FunctionDef,
    ctx: &mut ExecutionContext,
    agent_executor: &dyn AgentExecutor,
    func_executor: &dyn FunctionExecutor,
) -> MechResult<JsonValue> {
    let terminals = find_dataflow_terminals(function)?;
    let reachable = backward_reachable(&function.blocks, &terminals);
    let levels = topo_sort_levels(&function.blocks, &reachable)?;

    // Execute level by level.
    for level in &levels {
        for block_id in level {
            execute_block(
                workflow,
                function,
                block_id,
                ctx,
                agent_executor,
                func_executor,
            )
            .await?;
        }
    }

    // Collect terminal outputs.
    if terminals.len() == 1 {
        // Single terminal: return its output directly.
        let output = ctx.get_block_output(&terminals[0])?;
        Ok(output.clone())
    } else {
        // Multiple terminals (dataflow sinks): collect into a map.
        let mut map = serde_json::Map::with_capacity(terminals.len());
        for name in &terminals {
            let output = ctx.get_block_output(name)?;
            map.insert(name.clone(), output.clone());
        }
        Ok(JsonValue::Object(map))
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::WorkflowState;
    use crate::exec::agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
    use crate::exec::call::FunctionExecutor;
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

    struct NoFuncExecutor;

    impl FunctionExecutor for NoFuncExecutor {
        fn call<'a>(
            &'a self,
            function_name: &'a str,
            _input: JsonValue,
        ) -> BoxFuture<'a, Result<JsonValue, MechError>> {
            Box::pin(async move {
                Err(MechError::WorkflowValidation {
                    errors: vec![format!("unexpected call to `{function_name}`")],
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

    fn new_ctx(input: JsonValue) -> ExecutionContext {
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        ExecutionContext::new(input, json!({ "run_id": "r1" }), &BTreeMap::new(), ws).unwrap()
    }

    // ---- T1: Simple DAG (A, B roots → C depends on both) -----------------

    const SIMPLE_DAG: &str = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
      required: [result]
      properties: { result: { type: string } }
    blocks:
      a:
        prompt: "block a"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
      b:
        prompt: "block b"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
      c:
        prompt: "combine {{block.a.output.val}} and {{block.b.output.val}}"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
        depends_on: [a, b]
"#;

    #[test]
    fn simple_dag_executes_roots_before_dependent() {
        let wf = load(SIMPLE_DAG);
        let func = wf.document().functions.get("f").unwrap();
        // A and B are roots (level 0), C depends on both (level 1).
        let agent = SequentialAgent::new(vec![
            json!({ "val": "A_out" }),
            json!({ "val": "B_out" }),
            json!({ "result": "combined" }),
        ]);
        let mut ctx = new_ctx(json!({}));

        let out = run_blocking(run_function_dataflow(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &NoFuncExecutor,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": "combined" }));
    }

    // ---- T2: Diamond dependency (A→B, A→C, B→D, C→D) ---------------------

    const DIAMOND: &str = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
      required: [merged]
      properties: { merged: { type: string } }
    blocks:
      a:
        prompt: "root"
        schema:
          type: object
          required: [x]
          properties: { x: { type: integer } }
      b:
        prompt: "left {{block.a.output.x}}"
        schema:
          type: object
          required: [left]
          properties: { left: { type: string } }
        depends_on: [a]
      c:
        prompt: "right {{block.a.output.x}}"
        schema:
          type: object
          required: [right]
          properties: { right: { type: string } }
        depends_on: [a]
      d:
        prompt: "merge {{block.b.output.left}} {{block.c.output.right}}"
        schema:
          type: object
          required: [merged]
          properties: { merged: { type: string } }
        depends_on: [b, c]
"#;

    #[test]
    fn diamond_dependency_executes_correctly() {
        let wf = load(DIAMOND);
        let func = wf.document().functions.get("f").unwrap();
        // Level 0: a; Level 1: b, c; Level 2: d
        let agent = SequentialAgent::new(vec![
            json!({ "x": 42 }),
            json!({ "left": "L" }),
            json!({ "right": "R" }),
            json!({ "merged": "L+R" }),
        ]);
        let mut ctx = new_ctx(json!({}));

        let out = run_blocking(run_function_dataflow(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &NoFuncExecutor,
        ))
        .unwrap();

        assert_eq!(out, json!({ "merged": "L+R" }));
        // Root block `a` executed exactly once (only 4 total agent calls).
    }

    // ---- T3: Unreachable nodes not executed --------------------------------

    #[test]
    fn unreachable_nodes_not_executed() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [b]
    blocks:
      a:
        prompt: "root"
        schema:
          type: object
          required: [x]
          properties: { x: { type: integer } }
      b:
        prompt: "reachable"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
        depends_on: [a]
      orphan:
        prompt: "should not run"
        schema:
          type: object
          required: [y]
          properties: { y: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        // Only 2 agent calls: a then b. orphan is never reached.
        let agent = SequentialAgent::new(vec![json!({ "x": 1 }), json!({ "result": "done" })]);
        let mut ctx = new_ctx(json!({}));

        let out = run_blocking(run_function_dataflow(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &NoFuncExecutor,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": "done" }));
        // orphan was never executed — accessing its output would error.
        assert!(ctx.get_block_output("orphan").is_err());
    }

    // ---- T4: Multiple terminal blocks → map output ------------------------

    const MULTI_TERMINAL: &str = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
    blocks:
      shared:
        prompt: "shared root"
        schema:
          type: object
          required: [data]
          properties: { data: { type: string } }
      sink_a:
        prompt: "sink a: {{block.shared.output.data}}"
        schema:
          type: object
          required: [a_result]
          properties: { a_result: { type: string } }
        depends_on: [shared]
      sink_b:
        prompt: "sink b: {{block.shared.output.data}}"
        schema:
          type: object
          required: [b_result]
          properties: { b_result: { type: string } }
        depends_on: [shared]
"#;

    #[test]
    fn multiple_terminals_produce_map_output() {
        let wf = load(MULTI_TERMINAL);
        let func = wf.document().functions.get("f").unwrap();
        // Level 0: shared; Level 1: sink_a, sink_b (both terminals)
        let agent = SequentialAgent::new(vec![
            json!({ "data": "root_data" }),
            json!({ "a_result": "from_a" }),
            json!({ "b_result": "from_b" }),
        ]);
        let mut ctx = new_ctx(json!({}));

        let out = run_blocking(run_function_dataflow(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &NoFuncExecutor,
        ))
        .unwrap();

        // Multiple terminals → map keyed by block name.
        assert_eq!(
            out,
            json!({
                "sink_a": { "a_result": "from_a" },
                "sink_b": { "b_result": "from_b" }
            })
        );
    }

    // ---- T5: Terminal detection with explicit terminals --------------------

    #[test]
    fn explicit_terminals_override_auto_detection() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    terminals: [b]
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
          required: [y]
          properties: { y: { type: string } }
        depends_on: [a]
      c:
        prompt: "c"
        schema:
          type: object
          required: [z]
          properties: { z: { type: string } }
        depends_on: [a]
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let terminals = find_dataflow_terminals(func).unwrap();
        assert_eq!(terminals, vec!["b".to_string()]);
    }

    // ---- T6: Topo sort produces correct levels ----------------------------

    #[test]
    fn topo_sort_diamond_produces_three_levels() {
        let wf = load(DIAMOND);
        let func = wf.document().functions.get("f").unwrap();
        let terminals = find_dataflow_terminals(func).unwrap();
        let reachable = backward_reachable(&func.blocks, &terminals);
        let levels = topo_sort_levels(&func.blocks, &reachable).unwrap();

        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["a".to_string()]);
        // b and c are in level 1 (BTreeMap ordering: b < c).
        assert_eq!(levels[1].len(), 2);
        assert!(levels[1].contains(&"b".to_string()));
        assert!(levels[1].contains(&"c".to_string()));
        assert_eq!(levels[2], vec!["d".to_string()]);
    }

    // ---- T7: Single block dataflow ----------------------------------------

    #[test]
    fn single_block_dataflow() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      only:
        prompt: "hello"
        schema:
          type: object
          required: [answer]
          properties: { answer: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "answer": "42" })]);
        let mut ctx = new_ctx(json!({}));

        let out = run_blocking(run_function_dataflow(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &NoFuncExecutor,
        ))
        .unwrap();

        assert_eq!(out, json!({ "answer": "42" }));
    }

    // ---- T8: Dataflow with set_context ------------------------------------

    #[test]
    fn dataflow_applies_side_effects() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
      required: [result]
      properties: { result: { type: integer } }
    context:
      total: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [val]
          properties: { val: { type: integer } }
        set_context:
          total: "output.val"
      b:
        prompt: "b with {{context.total}}"
        schema:
          type: object
          required: [result]
          properties: { result: { type: integer } }
        depends_on: [a]
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "val": 10 }), json!({ "result": 20 })]);
        let fn_decls = func.context.clone();
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        let mut ctx =
            ExecutionContext::new(json!({}), json!({ "run_id": "r1" }), &fn_decls, ws).unwrap();

        let out = run_blocking(run_function_dataflow(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &NoFuncExecutor,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": 20 }));
        assert_eq!(ctx.get_context("total"), Some(&json!(10)));
    }
}
