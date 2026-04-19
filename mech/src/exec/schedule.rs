//! Transition evaluation and block scheduling, including conversation
//! scoping for prompt blocks.
//!
//! Implements imperative-mode function execution: starting at the entry block,
//! execute block → stage `set_context` / `set_workflow` side-effects →
//! commit them → evaluate transitions (rolling back the commits if
//! transition evaluation errors) → advance to the next block, until a
//! terminal block is reached.
//!
//! Per `docs/MECH_SPEC.md`:
//! - §6.2: transitions evaluated top-to-bottom, first match wins
//! - §6.3: guards have access to `output`, `input`, `context`, `workflow`
//! - §6.4: self-loops and backward edges are permitted
//! - §6.5: no matching transition → de facto terminal
//! - §9.3: `set_context` / `set_workflow` evaluated atomically, applied
//!   before transitions
//! - §9.3 rule 7: a guard error after commit rolls back this block's
//!   `set_context` / `set_workflow` writes (per-touched-key) before the
//!   error propagates
//! - §10.2: guard evaluation error → block failure (MechError::GuardEvaluationError)

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as JsonValue;

use crate::cel::{Namespaces, cel_value_to_json};
use crate::context::ExecutionContext;
use crate::conversation::Conversation;
use crate::error::{MechError, MechResult};
use crate::exec::agent::AgentExecutor;
use crate::exec::call::{FunctionExecutor, execute_call_block};
use crate::exec::prompt::execute_prompt_block;
use crate::schema::{BlockDef, FunctionDef, TransitionDef};
use crate::validate::graph::compute_dominators_with_entry;
use crate::workflow::Workflow;
const MAX_IMPERATIVE_STEPS: usize = 10_000;

/// Result of evaluating transitions for a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionResult {
    /// A transition matched; advance to this block.
    Goto(String),
    /// No transition matched (or no transitions declared); block is terminal.
    Terminal,
}

/// Build post-block namespaces that include `output` as an extra variable.
///
/// Transition guards and `set_context` / `set_workflow` expressions have
/// access to `output`, `input`, `context`, `workflow`, and `meta` — but NOT
/// `blocks.*` per §6.3. We keep `block` in the namespaces for simplicity
/// since the validator already rejects guard references to `blocks.*`.
fn build_post_block_namespaces(ctx: &ExecutionContext, output: &JsonValue) -> Namespaces {
    let base = ctx.namespaces();
    let mut extras = BTreeMap::new();
    extras.insert("output".to_string(), output.clone());
    Namespaces::with_extras(
        base.input,
        base.context,
        base.workflow,
        base.blocks,
        base.meta,
        extras,
    )
}

/// Evaluate outgoing transitions for a block in declaration order.
///
/// Returns the first matching target or [`TransitionResult::Terminal`]. A
/// guard that errors during evaluation surfaces as
/// [`MechError::GuardEvaluationError`] per spec §10.2 — silently treating
/// such errors as false would mask author bugs and let execution drift into
/// the wrong branch.
pub fn evaluate_transitions(
    workflow: &Workflow,
    block_id: &str,
    transitions: &[TransitionDef],
    output: &JsonValue,
    ctx: &ExecutionContext,
) -> MechResult<TransitionResult> {
    if transitions.is_empty() {
        return Ok(TransitionResult::Terminal);
    }

    let ns = build_post_block_namespaces(ctx, output);

    for t in transitions {
        match &t.when {
            None => {
                // Unconditional — always matches.
                return Ok(TransitionResult::Goto(t.goto.clone()));
            }
            Some(guard_src) => {
                let cel_expr = workflow.cel_expression(guard_src).ok_or_else(|| {
                    // Loader invariant: every guard is compiled at load time. A
                    // miss here is a runtime corruption of the Workflow handle.
                    MechError::InternalInvariant {
                        message: format!(
                            "guard `{guard_src}` should have been compiled at load time"
                        ),
                    }
                })?;
                match cel_expr.evaluate_guard(&ns) {
                    Ok(true) => return Ok(TransitionResult::Goto(t.goto.clone())),
                    Ok(false) => continue,
                    Err(e) => {
                        // Spec §10.2: guard evaluation error is a block-level failure.
                        return Err(MechError::GuardEvaluationError {
                            block: block_id.to_string(),
                            expression: guard_src.clone(),
                            message: e.to_string(),
                        });
                    }
                }
            }
        }
    }

    // No transition matched.
    Ok(TransitionResult::Terminal)
}

/// Staged side-effects ready to commit: `(var_name, computed_value)` pairs
/// for `set_context` (`.0`) and `set_workflow` (`.1`).
pub(crate) struct StagedSideEffects {
    context_writes: Vec<(String, JsonValue)>,
    workflow_writes: Vec<(String, JsonValue)>,
}

/// Evaluate `set_context` and `set_workflow` expressions WITHOUT applying
/// any writes. Per §9.3 expressions within each field are evaluated
/// atomically against the pre-write state. Returns a [`StagedSideEffects`]
/// the caller may choose to commit (via [`commit_side_effects`]) or
/// discard.
///
/// This function only stages; the decision to commit, snapshot for
/// rollback, evaluate transitions, or roll back belongs to the caller.
/// See [`commit_block_side_effects_then_evaluate`] for the canonical
/// orchestration used by [`run_function_imperative`].
pub(crate) fn stage_side_effects(
    workflow: &Workflow,
    set_context: &BTreeMap<String, String>,
    set_workflow: &BTreeMap<String, String>,
    output: &JsonValue,
    ctx: &ExecutionContext,
) -> MechResult<StagedSideEffects> {
    let ns = build_post_block_namespaces(ctx, output);

    // Evaluate all set_context expressions atomically (all see pre-write state).
    let mut context_writes: Vec<(String, JsonValue)> = Vec::with_capacity(set_context.len());
    for (var_name, expr_src) in set_context {
        let cel_expr =
            workflow
                .cel_expression(expr_src)
                .ok_or_else(|| MechError::InternalInvariant {
                    message: format!(
                        "set_context expression `{expr_src}` should have been compiled at load time"
                    ),
                })?;
        let cel_value = cel_expr.evaluate(&ns)?;
        let json_value = cel_value_to_json(&cel_value)?;
        context_writes.push((var_name.clone(), json_value));
    }

    // Evaluate all set_workflow expressions atomically (all see pre-write state).
    let mut workflow_writes: Vec<(String, JsonValue)> = Vec::with_capacity(set_workflow.len());
    for (var_name, expr_src) in set_workflow {
        let cel_expr = workflow.cel_expression(expr_src).ok_or_else(|| {
            MechError::InternalInvariant {
                message: format!(
                    "set_workflow expression `{expr_src}` should have been compiled at load time"
                ),
            }
        })?;
        let cel_value = cel_expr.evaluate(&ns)?;
        let json_value = cel_value_to_json(&cel_value)?;
        workflow_writes.push((var_name.clone(), json_value));
    }

    Ok(StagedSideEffects {
        context_writes,
        workflow_writes,
    })
}

/// Commit previously-staged side effects. Per §9.3 `set_context` writes are
/// applied first, then `set_workflow` writes.
///
/// **Caution:** on partial failure (e.g. a type-check failure on the
/// second of two writes after the first lands) the store is left
/// half-committed; this function does not snapshot or roll back. Callers
/// that need rollback semantics MUST snapshot via
/// [`snapshot_prior_values`] before invocation and call
/// [`restore_from_snapshot`] on `Err`. Today the only caller that does
/// this is [`commit_block_side_effects_then_evaluate`]; the dataflow
/// caller [`apply_side_effects`] deliberately does not — see its docs.
pub(crate) fn commit_side_effects(
    staged: StagedSideEffects,
    ctx: &mut ExecutionContext,
) -> MechResult<()> {
    for (name, value) in staged.context_writes {
        ctx.set_context(&name, value)?;
    }
    for (name, value) in staged.workflow_writes {
        ctx.set_workflow(&name, value)?;
    }
    Ok(())
}

/// Apply `set_context` and `set_workflow` side-effects after a block produces
/// output. Stages then immediately commits with no rollback path — only the
/// imperative scheduler wires rollback through [`stage_side_effects`] +
/// [`commit_side_effects`] + [`snapshot_prior_values`] /
/// [`restore_from_snapshot`], because dataflow blocks have no transition
/// guards and therefore cannot trigger §9.3 rule 7 (rollback on guard
/// error).
///
/// Per §9.3, expressions within each field are evaluated atomically against
/// the pre-write state (rule 3), and `set_context` writes are applied
/// before `set_workflow` writes (rule 4).
///
/// Dataflow blocks declare no transitions, so there is no post-commit guard
/// step here and nothing to roll back.
///
/// **Partial-commit acceptance (dataflow path):** the dataflow path
/// accepts partial-commit on commit failure because (a) dataflow blocks
/// have no transitions to evaluate and the function will fail outright on
/// commit error, and (b) §10.x dataflow failure semantics already treat
/// the function as failed; rollback would not change the observable
/// outcome. This is the deliberate divergence from the imperative path
/// flagged in [`commit_side_effects`]'s caution note.
pub fn apply_side_effects(
    workflow: &Workflow,
    _block_id: &str,
    set_context: &BTreeMap<String, String>,
    set_workflow: &BTreeMap<String, String>,
    output: &JsonValue,
    ctx: &mut ExecutionContext,
) -> MechResult<()> {
    let staged = stage_side_effects(workflow, set_context, set_workflow, output, ctx)?;
    commit_side_effects(staged, ctx)
}

/// Find the entry block for imperative-mode execution.
///
/// The entry block is the block with no `depends_on` that is not targeted
/// by any *other* block's transitions (self-loops are excluded). If all
/// non-depends_on blocks are transition targets (e.g. backward edges), fall
/// back to the first non-depends_on block in iteration order.
fn find_entry_block(function: &FunctionDef) -> MechResult<String> {
    // Collect all blocks that are transition targets from a DIFFERENT block.
    let mut targeted: BTreeSet<&str> = BTreeSet::new();
    for (src_name, block) in &function.blocks {
        for t in block.transitions() {
            if t.goto != *src_name {
                targeted.insert(&t.goto);
            }
        }
    }

    // Find blocks with no depends_on.
    let mut no_deps: Vec<&str> = Vec::new();
    for (name, block) in &function.blocks {
        let has_depends = !block.depends_on().is_empty();
        if !has_depends {
            no_deps.push(name);
        }
    }

    if no_deps.is_empty() {
        return Err(MechError::WorkflowValidation {
            errors: vec!["no entry block found: every block has depends_on".into()],
        });
    }

    // Prefer blocks with no inbound transitions from other blocks.
    let non_targeted: Vec<&str> = no_deps
        .iter()
        .filter(|name| !targeted.contains(**name))
        .copied()
        .collect();

    if non_targeted.is_empty() {
        // All non-deps blocks are transition targets (backward edges).
        // Fall back to first in iteration order.
        Ok(no_deps[0].to_string())
    } else {
        Ok(non_targeted[0].to_string())
    }
}

/// Snapshot of pre-commit values for the keys touched by a single block's
/// staged side effects. Captured per-key (not whole-state) so that
/// rollback after a guard error does not clobber concurrent
/// `set_workflow` writes from sibling parallel function invocations
/// (spec §9.6).
struct PreCommitSnapshot {
    /// Pre-commit `context.*` values for the keys this block writes.
    context_prior: BTreeMap<String, JsonValue>,
    /// Pre-commit `workflow.*` values for the keys this block writes.
    workflow_prior: BTreeMap<String, JsonValue>,
}

/// Capture the pre-commit values of exactly the keys appearing in
/// `staged`. Must be called BEFORE [`commit_side_effects`].
///
/// Limited to the touched-key set per §9.6: a function-context snapshot
/// could safely capture the whole map (per-invocation), but workflow state
/// is shared with parallel sibling functions; restoring the whole snapshot
/// would clobber their concurrent writes.
///
/// Returns `Err(MechError::InternalInvariant)` if any touched key has no
/// current value: declared `context.*` keys are always initialised from
/// their `initial`, and `set_workflow` targets are validated against
/// `workflow.context` declarations at load time, so absence here means
/// the underlying store is corrupt rather than a recoverable runtime
/// condition.
fn snapshot_prior_values(
    staged: &StagedSideEffects,
    ctx: &ExecutionContext,
) -> MechResult<PreCommitSnapshot> {
    let mut context_prior: BTreeMap<String, JsonValue> = BTreeMap::new();
    for (name, _) in &staged.context_writes {
        let v = ctx
            .get_context(name)
            .ok_or_else(|| MechError::InternalInvariant {
                message: format!(
                    "snapshot: declared context variable `{name}` has no current value"
                ),
            })?;
        context_prior.insert(name.clone(), v.clone());
    }
    let mut workflow_prior: BTreeMap<String, JsonValue> = BTreeMap::new();
    let ws = ctx.workflow_state();
    for (name, _) in &staged.workflow_writes {
        let v = ws.get(name).ok_or_else(|| MechError::InternalInvariant {
            message: format!("snapshot: declared workflow variable `{name}` has no current value"),
        })?;
        workflow_prior.insert(name.clone(), v);
    }
    Ok(PreCommitSnapshot {
        context_prior,
        workflow_prior,
    })
}

/// Restore the values captured by [`snapshot_prior_values`]. Used on the
/// commit-failure and guard-error paths only — successful transitions
/// retain the writes.
///
/// Best-effort: attempts every restoration regardless of intermediate
/// failure; aggregates all errors into a single `MechError::InternalInvariant`.
/// The function is documented to fire only on store corruption (the values
/// being restored were already type-correct when captured), so abandoning
/// half the work on the first failure would only worsen recovery
/// diagnostics.
///
/// Restoration order matches commit order (context first, then workflow)
/// rather than reverse order: per §9.3 rule 4 the two stores are
/// independent at the per-key level so order does not affect correctness;
/// matching commit order keeps the rollback path symmetric with the commit
/// path it undoes.
///
/// The values being restored were already type-correct when captured (we
/// just read them out of the same store), so `set_context` /
/// `set_workflow` should not fail. If they do, surface as
/// `MechError::InternalInvariant` rather than swallowing — that indicates
/// a corrupted declaration table or a concurrent declaration mutation,
/// not a recoverable workflow condition.
///
/// **Parallel-sibling caveats (future work):** once within-level
/// parallelism lands, three sibling-interaction hazards apply. (1) For
/// keys this block touched, a sibling commit between this block's
/// snapshot and rollback is clobbered by the rollback (same-key
/// last-write-wins per §9.6). (2) On commit-failure rollback,
/// `restore_from_snapshot` writes back EVERY captured snapshot value,
/// including for keys whose commit attempt was never reached, so sibling
/// writes to those untouched-by-this-block keys are also clobbered. (3)
/// The snapshot itself is non-atomic: per-key reads release the workflow
/// mutex between keys, so a captured snapshot may already mix pre- and
/// post-sibling-write values across different keys.
// TODO(parallel-siblings): revisit when within-level parallelism lands;
// see §9.6.
fn restore_from_snapshot(
    snapshot: PreCommitSnapshot,
    ctx: &mut ExecutionContext,
) -> MechResult<()> {
    let mut failures: Vec<String> = Vec::new();
    for (name, value) in snapshot.context_prior {
        if let Err(e) = ctx.set_context(&name, value) {
            failures.push(format!(
                "failed to restore context.{name} during rollback: {e}"
            ));
        }
    }
    for (name, value) in snapshot.workflow_prior {
        if let Err(e) = ctx.set_workflow(&name, value) {
            failures.push(format!(
                "failed to restore workflow.{name} during rollback: {e}"
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(MechError::InternalInvariant {
            message: format!("rollback: {}", failures.join("; ")),
        })
    }
}

/// Stage → snapshot → commit (rollback on failure) → evaluate transitions
/// (rollback on failure) → return the [`TransitionResult`].
///
/// This is the canonical post-block side-effect orchestration used by
/// [`run_function_imperative`]. Per §9.3 rule 4 `set_context` is applied
/// before `set_workflow` and transitions are evaluated after both
/// complete (so guards observe the post-write state — e.g. the §9.4
/// `attempts` counter pattern). Per §9.3 rule 7, if commit itself fails
/// part-way (e.g. type-check failure on the second of two writes) or if a
/// post-commit transition guard errors, this block's writes are rolled
/// back to their pre-commit values via [`restore_from_snapshot`] before
/// the original error propagates. Snapshot scope is per-touched-key so
/// the rollback does not clobber concurrent `set_workflow` writes from
/// parallel sibling functions for *different* keys (§9.6); see the
/// lost-update caveat on [`restore_from_snapshot`] for the same-key case.
///
/// Successful transition evaluation retains the writes — the §9.4 retry
/// counter pattern is unaffected.
///
/// **Rollback scope (intentional asymmetry):** rollback covers
/// `set_context` and `set_workflow` writes only. Block output recording
/// (in `ctx.block_outputs`) and conversation message append (for prompt
/// blocks) happen during block execution, BEFORE this helper is called,
/// and are intentionally NOT rolled back. The function returns `Err`, so
/// the caller should treat the partial state as opaque; subsequent block
/// visits (under cue retry, dominator-driven output clearing, etc.)
/// re-establish the right state. A future reader extending the rollback
/// set should note that the conversation in particular is not unwindable
/// — there is no "pop the last N messages" hook on `Conversation`.
fn commit_block_side_effects_then_evaluate(
    workflow: &Workflow,
    block_id: &str,
    transitions: &[TransitionDef],
    set_context: &BTreeMap<String, String>,
    set_workflow: &BTreeMap<String, String>,
    output: &JsonValue,
    ctx: &mut ExecutionContext,
) -> MechResult<TransitionResult> {
    let staged = stage_side_effects(workflow, set_context, set_workflow, output, ctx)?;
    let snapshot = snapshot_prior_values(&staged, ctx)?;

    // Commit the staged writes. If commit fails part-way (e.g. a type-check
    // failure on the second of two writes), roll back any partial writes
    // before propagating the commit error so direct callers do not observe
    // partial side effects (§9.3 rule 7).
    if let Err(commit_err) = commit_side_effects(staged, ctx) {
        if let Err(restore_err) = restore_from_snapshot(snapshot, ctx) {
            // Preserve the original commit error as the user-facing root
            // cause.
            // TODO(logging-facade): surface the rollback failure via tracing
            // once mech adopts a logging facade; eprintln! is the interim
            // mechanism.
            eprintln!(
                "warning: rollback after commit failure in block `{block_id}` failed: \
                 {restore_err}; original commit error: {commit_err}"
            );
        }
        return Err(commit_err);
    }

    // Evaluate transitions; on ANY error from evaluation (not just
    // GuardEvaluationError — InternalInvariant from a missing pre-compiled
    // guard expression also lands here) roll back the writes we just
    // committed for this block before propagating the original error. The
    // user-facing root cause is the transition error, not any failure of
    // the rollback itself.
    match evaluate_transitions(workflow, block_id, transitions, output, ctx) {
        Ok(r) => Ok(r),
        Err(e) => {
            if let Err(restore_err) = restore_from_snapshot(snapshot, ctx) {
                // Preserve the original transition error as the user-facing
                // root cause.
                // TODO(logging-facade): surface the rollback failure via
                // tracing once mech adopts a logging facade; eprintln! is the
                // interim mechanism.
                eprintln!(
                    "warning: rollback after transition error in block `{block_id}` failed: \
                     {restore_err}; original transition error: {e}"
                );
            }
            Err(e)
        }
    }
}

/// Run a single function to completion in imperative mode.
///
/// Starts at the entry block, executes block → side effects → transitions →
/// next block until a terminal block is reached. Returns the terminal block's
/// output.
///
/// Per §9.3 rule 4 `set_context` writes are applied first, then
/// `set_workflow` writes; transitions are evaluated after both complete.
/// Per §9.3 rule 7, if the post-commit transition guard errors (or commit
/// itself fails part-way) the block's writes are rolled back to their
/// pre-commit values before the error propagates, so direct callers
/// observe a clean pre-block state on `Err(GuardEvaluationError)`. The
/// rollback is per-touched-key so it preserves concurrent
/// `set_workflow` writes to *different* keys from parallel sibling
/// functions (§9.6); see the lost-update caveat on
/// [`restore_from_snapshot`] for the same-key case. Successful transitions
/// retain the writes (the §9.4 retry counter pattern is unaffected). Cue
/// retry isolation via a fresh `WorkflowState` per attempt is
/// belt-and-braces; direct callers of `run_function_imperative` also
/// observe clean state on guard error.
///
/// Per §4.6, a function's conversation is created fresh at invocation and
/// accumulates across prompt blocks along control-flow paths. Call blocks
/// are conversation-transparent.
#[allow(clippy::too_many_arguments)]
pub async fn run_function_imperative(
    workflow: &Workflow,
    function_name: &str,
    function: &FunctionDef,
    ctx: &mut ExecutionContext,
    agent_executor: &dyn AgentExecutor,
    func_executor: &dyn FunctionExecutor,
    conversation: &mut Conversation,
    rendered_system: Option<&str>,
) -> MechResult<JsonValue> {
    let entry = find_entry_block(function)?;
    // Compute dominators once per invocation so that each transition can
    // efficiently determine which block outputs remain in scope.
    let dominators = compute_dominators_with_entry(function, &entry);
    let mut current_block_id = entry;

    let mut step_count: usize = 0;
    loop {
        step_count += 1;
        if step_count > MAX_IMPERATIVE_STEPS {
            return Err(MechError::WorkflowValidation {
                errors: vec![format!(
                    "function `{function_name}`: exceeded maximum step count of \n                     {MAX_IMPERATIVE_STEPS}; possible infinite loop (self-loop guard never terminates)"
                )],
            });
        }

        let block = function.blocks.get(&current_block_id).ok_or_else(|| {
            MechError::WorkflowValidation {
                errors: vec![format!(
                    "function `{function_name}`: block `{current_block_id}` not found"
                )],
            }
        })?;

        // Execute the block.
        let output = match block {
            BlockDef::Prompt(p) => {
                execute_prompt_block(
                    workflow,
                    function,
                    &current_block_id,
                    p,
                    ctx,
                    agent_executor,
                    conversation,
                    rendered_system,
                )
                .await?
            }
            BlockDef::Call(c) => {
                // Call blocks are conversation-transparent (§4.6 rule 4).
                execute_call_block(workflow, function, &current_block_id, c, ctx, func_executor)
                    .await?
            }
        };

        // Per §9.3 rule 7 the side-effect / transition dance must roll back
        // this block's writes if commit or transition evaluation errors,
        // before the error propagates. The orchestration is encapsulated in
        // `commit_block_side_effects_then_evaluate`.
        let common = block.common();
        let result = commit_block_side_effects_then_evaluate(
            workflow,
            &current_block_id,
            &common.transitions,
            &common.set_context,
            &common.set_workflow,
            &output,
            ctx,
        )?;

        match result {
            TransitionResult::Terminal => return Ok(output),
            TransitionResult::Goto(next) => {
                // C-26: when transitioning to `next`, preserve only the
                // outputs of blocks that *strictly* dominate `next` — i.e.
                // blocks on every path from entry to `next`.  All other
                // recorded outputs are stale: they come from abandoned
                // branches, prior-iteration siblings, or `next` itself
                // (which must re-execute).  Clear them all.
                //
                // Strict dominators of `next` = dom[next] \ {next}.
                let empty_dom_set = BTreeSet::new();
                let doms_of_next = dominators.get(&next).unwrap_or(&empty_dom_set);
                for block_id in function.blocks.keys() {
                    // Keep output iff block_id strictly dominates `next`.
                    if block_id != &next && doms_of_next.contains(block_id.as_str()) {
                        continue;
                    }
                    ctx.clear_block_output(block_id);
                }
                current_block_id = next;
            }
        }
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, WorkflowState};
    use crate::conversation::Conversation;
    use crate::exec::agent::{AgentExecutor, AgentRequest, AgentResponse, BoxFuture};
    use crate::exec::call::FunctionExecutor;
    use crate::exec::test_support::{CapturingAgent, assert_all_requests_have_system};
    use crate::loader::WorkflowLoader;
    use crate::schema::ContextVarDef;
    use serde_json::json;
    use std::sync::Mutex;

    // ---- Test helpers -----------------------------------------------------

    /// Agent that returns responses from a queue in order.
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

    /// Function executor that returns canned responses by name.
    struct FakeFuncExecutor {
        responses: BTreeMap<String, JsonValue>,
    }

    impl FakeFuncExecutor {
        fn new(responses: BTreeMap<String, JsonValue>) -> Self {
            Self { responses }
        }
    }

    impl FunctionExecutor for FakeFuncExecutor {
        fn call<'a>(
            &'a self,
            function_name: &'a str,
            _input: JsonValue,
        ) -> BoxFuture<'a, Result<JsonValue, MechError>> {
            let result = self.responses.get(function_name).cloned().ok_or_else(|| {
                MechError::WorkflowValidation {
                    errors: vec![format!("fake: no response for `{function_name}`")],
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

    fn load(yaml: &str) -> Workflow {
        WorkflowLoader::new().load_str(yaml).expect("load")
    }

    fn new_ctx(
        input: JsonValue,
        fn_decls: &BTreeMap<String, ContextVarDef>,
        wf_decls: &BTreeMap<String, ContextVarDef>,
    ) -> ExecutionContext {
        let ws = WorkflowState::from_declarations(wf_decls).unwrap();
        ExecutionContext::new(input, json!({ "run_id": "r1" }), fn_decls, ws).unwrap()
    }

    fn new_ctx_with_workflow(
        input: JsonValue,
        fn_decls: &BTreeMap<String, ContextVarDef>,
        ws: WorkflowState,
    ) -> ExecutionContext {
        ExecutionContext::new(input, json!({ "run_id": "r1" }), fn_decls, ws).unwrap()
    }

    fn decl(ty: &str, initial: JsonValue) -> ContextVarDef {
        ContextVarDef {
            ty: ty.to_string(),
            initial,
        }
    }

    fn no_func_executor() -> FakeFuncExecutor {
        FakeFuncExecutor::new(BTreeMap::new())
    }

    // ---- Linear sequence A → B → C terminates at C ----

    const LINEAR: &str = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "block a"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - goto: b
      b:
        prompt: "block b"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - goto: c
      c:
        prompt: "block c"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
"#;

    #[test]
    fn linear_sequence_terminates_at_c() {
        let wf = load(LINEAR);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "val": "A" }),
            json!({ "val": "B" }),
            json!({ "val": "C" }),
        ]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "val": "C" }));
    }

    // ---- Guard selects among multiple transitions ----

    const GUARDED: &str = r#"
functions:
  f:
    input: { type: object }
    blocks:
      classify:
        prompt: "classify"
        schema:
          type: object
          required: [category]
          properties: { category: { type: string } }
        transitions:
          - when: 'output.category == "billing"'
            goto: billing
          - when: 'output.category == "technical"'
            goto: technical
          - goto: general
      billing:
        prompt: "billing"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
      technical:
        prompt: "technical"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
      general:
        prompt: "general"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;

    #[test]
    fn guard_selects_billing_branch() {
        let wf = load(GUARDED);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "category": "billing" }),
            json!({ "result": "billing handled" }),
        ]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": "billing handled" }));
    }

    #[test]
    fn guard_selects_technical_branch() {
        let wf = load(GUARDED);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "category": "technical" }),
            json!({ "result": "tech handled" }),
        ]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": "tech handled" }));
    }

    #[test]
    fn guard_falls_through_to_unconditional() {
        let wf = load(GUARDED);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "category": "other" }),
            json!({ "result": "general handled" }),
        ]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": "general handled" }));
    }

    // ---- Self-loop until guard flips ----

    const SELF_LOOP: &str = r#"
functions:
  f:
    input: { type: object }
    context:
      attempts: { type: integer, initial: 0 }
    blocks:
      draft:
        prompt: "draft attempt"
        schema:
          type: object
          required: [text, quality]
          properties:
            text: { type: string }
            quality: { type: number }
        set_context:
          attempts: "context.attempts + 1"
        transitions:
          - when: 'output.quality >= 0.8'
            goto: done
          - when: 'context.attempts < 3'
            goto: draft
          - goto: done
      done:
        prompt: "finalize"
        schema:
          type: object
          required: [final_text]
          properties: { final_text: { type: string } }
"#;

    #[test]
    fn self_loop_executes_until_guard_flips() {
        let wf = load(SELF_LOOP);
        let func = wf.document().functions.get("f").unwrap();
        // Three draft attempts with low quality, then done.
        let agent = SequentialAgent::new(vec![
            json!({ "text": "draft 1", "quality": 0.3 }),
            json!({ "text": "draft 2", "quality": 0.5 }),
            json!({ "text": "draft 3", "quality": 0.6 }),
            json!({ "final_text": "final" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("attempts".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "final_text": "final" }));
        // Attempts should be 3 (incremented each time draft executed).
        assert_eq!(ctx.get_context("attempts"), Some(&json!(3)));
    }

    #[test]
    fn self_loop_exits_early_on_quality() {
        let wf = load(SELF_LOOP);
        let func = wf.document().functions.get("f").unwrap();
        // First attempt has high quality — exits immediately.
        let agent = SequentialAgent::new(vec![
            json!({ "text": "great draft", "quality": 0.9 }),
            json!({ "final_text": "done" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("attempts".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "final_text": "done" }));
        assert_eq!(ctx.get_context("attempts"), Some(&json!(1)));
    }

    // ---- Terminal block (no transitions) ends function ----

    const SINGLE_BLOCK: &str = r#"
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

    #[test]
    fn terminal_block_ends_function() {
        let wf = load(SINGLE_BLOCK);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "answer": "42" })]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "answer": "42" }));
    }

    // ---- No matching guard, no fallback → terminal ----

    const NO_MATCH: &str = r#"
functions:
  f:
    input: { type: object }
    blocks:
      check:
        prompt: "check"
        schema:
          type: object
          required: [status]
          properties: { status: { type: string } }
        transitions:
          - when: 'output.status == "good"'
            goto: good
          - when: 'output.status == "bad"'
            goto: bad
      good:
        prompt: "good path"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
      bad:
        prompt: "bad path"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;

    #[test]
    fn no_matching_transition_is_terminal() {
        let wf = load(NO_MATCH);
        let func = wf.document().functions.get("f").unwrap();
        // Output status is "unknown" — matches no guard.
        let agent = SequentialAgent::new(vec![json!({ "status": "unknown" })]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        // Block becomes de facto terminal per §6.5.
        assert_eq!(out, json!({ "status": "unknown" }));
    }

    // ---- set_context reads output ----

    const SET_CONTEXT_OUTPUT: &str = r#"
functions:
  f:
    input: { type: object }
    context:
      score: { type: number, initial: 0.0 }
    blocks:
      compute:
        prompt: "compute"
        schema:
          type: object
          required: [value]
          properties: { value: { type: number } }
        set_context:
          score: "output.value"
"#;

    #[test]
    fn set_context_reads_output() {
        let wf = load(SET_CONTEXT_OUTPUT);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "value": 0.95 })]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("score".into(), decl("number", json!(0.0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(ctx.get_context("score"), Some(&json!(0.95)));
    }

    // ---- set_context atomicity (swap) ----

    const ATOMIC_SWAP: &str = r#"
functions:
  f:
    input: { type: object }
    context:
      a: { type: integer, initial: 1 }
      b: { type: integer, initial: 2 }
    blocks:
      swap:
        prompt: "trigger swap"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
        set_context:
          a: "context.b"
          b: "context.a"
"#;

    #[test]
    fn set_context_atomicity_swap() {
        let wf = load(ATOMIC_SWAP);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "ok": true })]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("a".into(), decl("integer", json!(1)));
        fn_decls.insert("b".into(), decl("integer", json!(2)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        // Both see pre-write state, so a gets old b (2) and b gets old a (1).
        assert_eq!(ctx.get_context("a"), Some(&json!(2)));
        assert_eq!(ctx.get_context("b"), Some(&json!(1)));
    }

    // ---- Guard evaluation error propagates as block failure --------------

    const GUARD_ERROR: &str = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
    blocks:
      check:
        prompt: "check"
        schema:
          type: object
          required: [status]
          properties: { status: { type: string } }
        transitions:
          # Uses `status` (a required string field) so the guard passes load-time
          # optional-field-safety validation; `.deep.field` still errors at runtime
          # because strings have no attributes.
          - when: 'output.status.deep.field == "x"'
            goto: unreachable
          - goto: fallback
      unreachable:
        prompt: "unreachable"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
      fallback:
        prompt: "fallback"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;

    // Regression: a guard whose evaluation errors must surface as
    // MechError::GuardEvaluationError, not be silently swallowed and let
    // execution drift to the next transition.
    #[test]
    fn guard_evaluation_error_propagates_as_block_failure() {
        let wf = load(GUARD_ERROR);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "status": "ok" })]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .expect_err("guard evaluation error must surface, not be silently swallowed");

        match err {
            MechError::GuardEvaluationError {
                block,
                expression,
                message,
            } => {
                assert_eq!(block, "check");
                assert!(
                    expression.contains("output.status.deep.field"),
                    "expected expression to mention `output.status.deep.field`, got: {expression}"
                );
                assert!(!message.is_empty(), "guard error message must be non-empty");
            }
            other => panic!("expected MechError::GuardEvaluationError, got {other:?}"),
        }
    }

    // `run_function_imperative` rolls back `set_context` writes made by a
    // block whose subsequent transition guard errors, so direct callers
    // (the function is `pub` and re-exported from `mech::lib`) observe the
    // pre-block context state on `Err(GuardEvaluationError)` rather than
    // partial-write side effects. The rollback is per-touched-key (not a
    // whole-context snapshot) so it cannot clobber concurrent
    // `set_workflow` writes from parallel sibling functions (§9.6).
    // Successful transitions still retain the writes — the §9.4 retry
    // counter pattern is unaffected.
    #[test]
    fn set_context_writes_rolled_back_on_guard_evaluation_error_in_run_function_imperative() {
        // the FIRST transition exists solely to falsify a no-op
        // commit. If `set_context` did not actually run, `context.attempts`
        // is still `0` and the function would transition to the terminal
        // `should_not_take_this` block (returning `Ok(...)` and breaking
        // `expect_err`). The expected path is: commit applies (attempts
        // becomes 1), the first guard is false, the second guard errors at
        // runtime, the scheduler rolls back, and we observe `attempts == 0`
        // again. This pins BOTH "commit was applied" AND "rollback ran".
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      attempts: { type: integer, initial: 0 }
    blocks:
      check:
        prompt: "check"
        schema:
          type: object
          required: [status]
          properties: { status: { type: string } }
        set_context:
          attempts: "context.attempts + 1"
        transitions:
          # Witness that commit happened: if it did not, `attempts == 0`
          # still holds and we'd take this branch instead of erroring.
          - when: 'context.attempts == 0'
            goto: should_not_take_this
          # Guard errors at runtime: strings have no attribute `.deep`.
          - when: 'output.status.deep.field == "x"'
            goto: unreachable
      should_not_take_this:
        prompt: "should not happen"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
      unreachable:
        prompt: "unreachable"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        // Two responses: one for `check`, one for the (only-reachable-if-
        // commit-was-noop) `should_not_take_this` block. The second is
        // never consumed on the expected path; it exists so the
        // falsification path returns `Ok(...)` cleanly rather than failing
        // on an exhausted-agent error (which would mask the real
        // "commit-was-noop" diagnosis).
        let agent = SequentialAgent::new(vec![
            json!({ "status": "ok" }),
            json!({ "r": "unexpected" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("attempts".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());
        let mut conv = Conversation::new(None);

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut conv,
            None,
        ))
        .expect_err("guard error must propagate");
        assert!(
            matches!(err, MechError::GuardEvaluationError { .. }),
            "expected GuardEvaluationError to propagate unchanged, got {err:?}"
        );

        // The `set_context` write `attempts: context.attempts + 1` committed
        // (so the post-write state was visible to the guard, per §9.4) but
        // because the guard then errored the scheduler restored the prior
        // value (the initial `0`). Direct callers therefore observe a clean
        // pre-block context.
        assert_eq!(
            ctx.get_context("attempts"),
            Some(&json!(0)),
            "set_context write must be rolled back when the same block's guard errors"
        );

        // conversation messages from the failing block are intentionally
        // NOT rolled back (see the rollback-asymmetry note on
        // `commit_block_side_effects_then_evaluate`). The block executed its
        // prompt, schema-validated the response, and committed both messages
        // (user prompt + assistant reply) BEFORE the side-effect/transition
        // dance ran. So we expect exactly 2 messages.
        assert_eq!(
            conv.len(),
            2,
            "conversation messages from the failing block must be retained \
             (per the rollback-asymmetry doc on commit_block_side_effects_then_evaluate)"
        );
    }

    // Sibling regression for `set_workflow`: the rollback path must restore
    // workflow-state writes too, otherwise a future change that wires
    // rollback for context only (or vice versa) would silently regress one
    // half. Read back through the `WorkflowState` handle directly so the
    // assertion does not depend on `ExecutionContext` plumbing.
    #[test]
    fn set_workflow_writes_rolled_back_on_guard_evaluation_error_in_run_function_imperative() {
        // see the comment on
        // `set_context_writes_rolled_back_on_guard_evaluation_error_in_run_function_imperative`
        // for the rationale behind the first transition. The mechanism here
        // reads `workflow.total` instead of `context.attempts`.
        let yaml = r#"
workflow:
  context:
    total: { type: integer, initial: 0 }
functions:
  f:
    input: { type: object }
    blocks:
      check:
        prompt: "check"
        schema:
          type: object
          required: [status]
          properties: { status: { type: string } }
        set_workflow:
          total: "workflow.total + 1"
        transitions:
          # Witness that commit happened: if it did not, `workflow.total ==
          # 0` still holds and we'd take this branch instead of erroring.
          - when: 'workflow.total == 0'
            goto: should_not_take_this
          # Guard errors at runtime: strings have no attribute `.deep`.
          - when: 'output.status.deep.field == "x"'
            goto: unreachable
      should_not_take_this:
        prompt: "should not happen"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
      unreachable:
        prompt: "unreachable"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "status": "ok" }),
            json!({ "r": "unexpected" }),
        ]);
        let mut wf_decls = BTreeMap::new();
        wf_decls.insert("total".into(), decl("integer", json!(0)));
        let ws = WorkflowState::from_declarations(&wf_decls).unwrap();
        let mut ctx = new_ctx_with_workflow(json!({}), &BTreeMap::new(), ws.clone());

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .expect_err("guard error must propagate");
        assert!(
            matches!(err, MechError::GuardEvaluationError { .. }),
            "expected GuardEvaluationError to propagate unchanged, got {err:?}"
        );

        // The `set_workflow` write `total: workflow.total + 1` committed
        // before the guard ran, then the guard errored. The scheduler must
        // have restored the prior workflow value via the `WorkflowState`
        // handle, so a sibling reader observes the initial `0` rather than
        // the partial increment.
        assert_eq!(
            ws.get("total"),
            Some(json!(0)),
            "set_workflow write must be rolled back when the same block's guard errors"
        );
    }

    // Both `set_context` AND `set_workflow` writes by the same block
    // must be rolled back together when that block's transition guard
    // errors. Pins that `restore_from_snapshot` runs both loops to
    // completion (regression guard for someone hoisting one out).
    #[test]
    fn both_context_and_workflow_writes_rolled_back_on_guard_evaluation_error_in_run_function_imperative()
     {
        // see the comment on
        // `set_context_writes_rolled_back_on_guard_evaluation_error_in_run_function_imperative`
        // for the rationale behind the first transition. The combined
        // disjunction here pins that BOTH writes committed before rollback
        // (a no-op on either alone would still satisfy `attempts == 0` or
        // `workflow.total == 0` and route to `should_not_take_this`).
        let yaml = r#"
workflow:
  context:
    total: { type: integer, initial: 0 }
functions:
  f:
    input: { type: object }
    context:
      attempts: { type: integer, initial: 0 }
    blocks:
      check:
        prompt: "check"
        schema:
          type: object
          required: [status]
          properties: { status: { type: string } }
        set_context:
          attempts: "context.attempts + 1"
        set_workflow:
          total: "workflow.total + 1"
        transitions:
          # Witness that BOTH commits happened: if either was a no-op the
          # corresponding sentinel still equals `0` and we'd take this
          # branch.
          - when: 'context.attempts == 0 || workflow.total == 0'
            goto: should_not_take_this
          # Guard errors at runtime: strings have no attribute `.deep`.
          - when: 'output.status.deep.field == "x"'
            goto: unreachable
      should_not_take_this:
        prompt: "should not happen"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
      unreachable:
        prompt: "unreachable"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "status": "ok" }),
            json!({ "r": "unexpected" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("attempts".into(), decl("integer", json!(0)));
        let mut wf_decls = BTreeMap::new();
        wf_decls.insert("total".into(), decl("integer", json!(0)));
        let ws = WorkflowState::from_declarations(&wf_decls).unwrap();
        let mut ctx = new_ctx_with_workflow(json!({}), &fn_decls, ws.clone());

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .expect_err("guard error must propagate");
        assert!(
            matches!(err, MechError::GuardEvaluationError { .. }),
            "expected GuardEvaluationError to propagate unchanged, got {err:?}"
        );

        // Both halves of the rollback must run.
        assert_eq!(
            ctx.get_context("attempts"),
            Some(&json!(0)),
            "set_context write must be rolled back alongside set_workflow"
        );
        assert_eq!(
            ws.get("total"),
            Some(json!(0)),
            "set_workflow write must be rolled back alongside set_context"
        );
    }

    // When a later block's guard errors, only THAT block's writes
    // are rolled back; writes committed by prior blocks on the executed
    // path must survive. Pins per-block snapshot scope (prevents a future
    // "snapshot at function entry" regression).
    #[test]
    fn prior_block_writes_survive_on_guard_evaluation_error_in_later_block_in_run_function_imperative()
     {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      x: { type: integer, initial: 0 }
      y: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "a"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
        set_context:
          x: "7"
        transitions:
          - goto: b
      b:
        prompt: "b"
        schema:
          type: object
          required: [status]
          properties: { status: { type: string } }
        set_context:
          y: "11"
        transitions:
          # Guard errors at runtime: strings have no attribute `.deep`.
          - when: 'output.status.deep.field == "x"'
            goto: unreachable
      unreachable:
        prompt: "unreachable"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "ok": true }), json!({ "status": "ok" })]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("x".into(), decl("integer", json!(0)));
        fn_decls.insert("y".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());
        let mut conv = Conversation::new(None);

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut conv,
            None,
        ))
        .expect_err("guard error in block b must propagate");
        assert!(
            matches!(err, MechError::GuardEvaluationError { .. }),
            "expected GuardEvaluationError, got {err:?}"
        );

        // Block A's write to `x` survives — its block boundary already
        // closed with a successful guard evaluation.
        assert_eq!(
            ctx.get_context("x"),
            Some(&json!(7)),
            "prior block A's write must NOT be rolled back when later block B's guard errors"
        );
        // Block B's write to `y` is rolled back to its initial.
        assert_eq!(
            ctx.get_context("y"),
            Some(&json!(0)),
            "block B's write must be rolled back because B's own guard errored"
        );

        // both blocks A and B executed their prompts and committed
        // their (user, assistant) message pairs to the conversation BEFORE
        // the side-effect/transition dance ran for each. Block B's guard
        // error rolls back B's `set_context` write but leaves the
        // conversation untouched (per the rollback-asymmetry note on
        // `commit_block_side_effects_then_evaluate`). Two blocks * two
        // messages each = 4.
        assert_eq!(
            conv.len(),
            4,
            "conversation messages from BOTH blocks (including the failing one) must be retained \
             (per the rollback-asymmetry doc on commit_block_side_effects_then_evaluate)"
        );
    }

    // When a guard errors on a block that wrote NEITHER set_context
    // NOR set_workflow, the rollback path is a no-op restore on an empty
    // snapshot. The original guard error must still propagate (not be
    // shadowed by anything from the empty rollback path).
    #[test]
    fn original_guard_error_propagates_even_if_no_rollback_needed_in_run_function_imperative() {
        // Reuse the GUARD_ERROR YAML: block `check` declares no
        // set_context / set_workflow and its guard errors at runtime.
        let wf = load(GUARD_ERROR);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "status": "ok" })]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .expect_err("guard error must surface even when there is nothing to roll back");
        // Mirrors the field-level checks in
        // `guard_evaluation_error_propagates_as_block_failure` (variant +
        // block + expression + non-empty message). The matched
        // `GuardEvaluationError` variant (rather than `InternalInvariant`)
        // is itself the assertion that the empty-snapshot rollback path
        // executed without producing its own error — a rollback failure
        // would surface as `InternalInvariant`.
        match err {
            MechError::GuardEvaluationError {
                block,
                expression,
                message,
            } => {
                assert_eq!(block, "check");
                assert!(
                    expression.contains("output.status.deep.field"),
                    "expected expression to mention `output.status.deep.field`, got: {expression}"
                );
                assert!(!message.is_empty(), "guard error message must be non-empty");
            }
            other => panic!(
                "expected MechError::GuardEvaluationError to propagate even with empty snapshot, got {other:?}"
            ),
        }
    }

    // a commit-time type-check failure on the second of two writes
    // exercises the commit-failure branch of
    // `commit_block_side_effects_then_evaluate` (not the post-commit
    // guard branch). `BTreeMap` iterates keys alphabetically, so naming
    // the type-correct write `var1` and the type-mismatched write `var2`
    // pins the order: `var1` commits, then `var2` fails its check inside
    // `commit_side_effects` (`MechError::WorkflowValidation` per
    // `ExecutionContext::set_context` -> `check_type` in
    // `mech/src/context.rs`). Rollback must restore `var1` to its
    // initial.
    #[test]
    fn commit_failure_triggers_rollback_in_run_function_imperative() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      var1: { type: integer, initial: 0 }
      var2: { type: integer, initial: 0 }
    blocks:
      check:
        prompt: "check"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
        set_context:
          # Alphabetical iteration: var1 commits first (integer 1, OK),
          # then var2 fails the integer type-check (string "foo").
          var1: "1"
          var2: '"foo"'
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "r": "ok" })]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("var1".into(), decl("integer", json!(0)));
        fn_decls.insert("var2".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let err = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .expect_err("commit-time type mismatch must propagate");

        // `set_context` raises `MechError::WorkflowValidation` on type
        // mismatch (see `check_type` in `mech/src/context.rs`). The
        // rollback path must NOT shadow this with `InternalInvariant`.
        assert!(
            matches!(err, MechError::WorkflowValidation { .. }),
            "expected WorkflowValidation from commit-time type-check failure, got {err:?}"
        );

        // var1's successful first write must be rolled back to its initial.
        assert_eq!(
            ctx.get_context("var1"),
            Some(&json!(0)),
            "var1's committed write must be rolled back when var2's commit fails"
        );
        // var2's commit was never reached, so it is still at its initial.
        assert_eq!(
            ctx.get_context("var2"),
            Some(&json!(0)),
            "var2 was never committed; it must remain at its initial value"
        );
    }

    // ---- set_context before set_workflow, transitions after both ----

    const SIDE_EFFECTS_ORDER: &str = r#"
workflow:
  context:
    wf_val: { type: integer, initial: 0 }
functions:
  f:
    input: { type: object }
    context:
      fn_val: { type: integer, initial: 0 }
    blocks:
      step:
        prompt: "step"
        schema:
          type: object
          required: [x]
          properties: { x: { type: integer } }
        set_context:
          fn_val: "output.x"
        set_workflow:
          wf_val: "output.x + 10"
        transitions:
          - when: 'context.fn_val > 0'
            goto: done
          - goto: step
      done:
        prompt: "done"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
"#;

    #[test]
    fn side_effects_applied_before_transitions() {
        let wf = load(SIDE_EFFECTS_ORDER);
        let func = wf.document().functions.get("f").unwrap();
        // First execution: output.x = 5. set_context sets fn_val = 5.
        // Transition sees context.fn_val = 5 > 0, goes to done.
        let agent = SequentialAgent::new(vec![json!({ "x": 5 }), json!({ "ok": true })]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("fn_val".into(), decl("integer", json!(0)));
        let mut wf_decls = BTreeMap::new();
        wf_decls.insert("wf_val".into(), decl("integer", json!(0)));
        let ws = WorkflowState::from_declarations(&wf_decls).unwrap();
        let mut ctx = new_ctx_with_workflow(json!({}), &fn_decls, ws.clone());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "ok": true }));
        assert_eq!(ctx.get_context("fn_val"), Some(&json!(5)));
        assert_eq!(ws.get("wf_val"), Some(json!(15)));
    }

    // ---- Backward edge (goto earlier block) ----

    const BACKWARD_EDGE: &str = r#"
functions:
  f:
    input: { type: object }
    context:
      rounds: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "block a"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        set_context:
          rounds: "context.rounds + 1"
        transitions:
          - goto: b
      b:
        prompt: "block b"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - when: 'context.rounds < 2'
            goto: a
          - goto: c
      c:
        prompt: "block c"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
"#;

    #[test]
    fn backward_edge_re_executes() {
        let wf = load(BACKWARD_EDGE);
        let func = wf.document().functions.get("f").unwrap();
        // Execution: a(round=1) → b → a(round=2) → b → c
        let agent = SequentialAgent::new(vec![
            json!({ "val": "a1" }),
            json!({ "val": "b1" }),
            json!({ "val": "a2" }),
            json!({ "val": "b2" }),
            json!({ "val": "c" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("rounds".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "val": "c" }));
        assert_eq!(ctx.get_context("rounds"), Some(&json!(2)));
    }

    // ---- Entry block detection ----

    #[test]
    fn entry_block_is_detected_correctly() {
        // In LINEAR, block "a" has no inbound transitions and no depends_on.
        let wf = load(LINEAR);
        let func = wf.document().functions.get("f").unwrap();
        let entry = find_entry_block(func).unwrap();
        assert_eq!(entry, "a");
    }

    #[test]
    fn entry_block_detection_with_depends_on() {
        // Block with depends_on is not an entry block.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      entry:
        prompt: "entry"
        schema:
          type: object
          required: [x]
          properties: { x: { type: string } }
        transitions:
          - goto: dependent
      dependent:
        prompt: "dependent"
        schema:
          type: object
          required: [y]
          properties: { y: { type: string } }
        depends_on: [entry]
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let entry = find_entry_block(func).unwrap();
        assert_eq!(entry, "entry");
    }

    // ---- evaluate_transitions unit tests ----

    #[test]
    fn evaluate_transitions_empty_is_terminal() {
        let wf = load(SINGLE_BLOCK);
        let ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());
        let result = evaluate_transitions(&wf, "only", &[], &json!({}), &ctx).unwrap();
        assert_eq!(result, TransitionResult::Terminal);
    }

    #[test]
    fn evaluate_transitions_unconditional_matches() {
        let wf = load(LINEAR);
        let ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());
        let transitions = vec![TransitionDef {
            when: None,
            goto: "b".into(),
        }];
        let result = evaluate_transitions(&wf, "a", &transitions, &json!({}), &ctx).unwrap();
        assert_eq!(result, TransitionResult::Goto("b".into()));
    }

    // When transitions exist but every guard evaluates to `Ok(false)`,
    // `evaluate_transitions` falls through the loop and returns
    // `Terminal`. The other three exit paths (empty list, unconditional
    // match, guard-eval error) are covered above; this test pins the
    // fourth.
    #[test]
    fn evaluate_transitions_all_guards_false_returns_terminal() {
        let ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());
        // Two guards that are constant-false. Both compile at load time
        // because they reference no fields, but they require an interned
        // CelExpression in the workflow handle. Use a bespoke YAML so the
        // intern set contains the guards we test against.
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
          - when: 'false'
            goto: never_a
          - when: '1 == 2'
            goto: never_b
      never_a:
        prompt: "never"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
      never_b:
        prompt: "never"
        schema:
          type: object
          required: [r]
          properties: { r: { type: string } }
"#;
        let wf2 = load(yaml);
        // Re-use the function's transitions from the loaded workflow.
        let func2 = wf2.document().functions.get("f").unwrap();
        let block_a = match &func2.blocks["a"] {
            BlockDef::Prompt(p) => p,
            _ => panic!("expected prompt"),
        };
        let result = evaluate_transitions(
            &wf2,
            "a",
            &block_a.common.transitions,
            &json!({ "x": "anything" }),
            &ctx,
        )
        .expect("evaluate_transitions must succeed when guards are well-formed");
        assert_eq!(
            result,
            TransitionResult::Terminal,
            "all guards false must fall through to Terminal"
        );
    }

    // Direct unit test for evaluate_transitions: a guard that errors at
    // CEL evaluation time (string has no  attribute) must
    // surface as MechError::GuardEvaluationError rather than being treated
    // as false. The integration-level test above covers the same path
    // through run_function_imperative; this one isolates evaluate_transitions.
    #[test]
    fn evaluate_transitions_propagates_guard_evaluation_error() {
        let wf = load(GUARD_ERROR);
        let ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());
        // Pull the transition list directly from the loaded workflow so this
        // test cannot drift out of sync with the YAML in `GUARD_ERROR`.
        // Re-constructing a `TransitionDef` by hand only works while the
        // guard string is byte-identical to the YAML — a whitespace edit in
        // `GUARD_ERROR` would cause the intern lookup to miss and the test
        // would surface `InternalInvariant` instead of the intended
        // `GuardEvaluationError`.
        let func = wf.document().functions.get("f").unwrap();
        let check_block = match &func.blocks["check"] {
            BlockDef::Prompt(p) => p,
            _ => panic!("expected prompt block"),
        };
        let transitions = check_block.common.transitions.clone();
        let err =
            evaluate_transitions(&wf, "check", &transitions, &json!({ "status": "ok" }), &ctx)
                .expect_err("guard evaluation error must propagate as Err");
        match err {
            MechError::GuardEvaluationError {
                block,
                expression,
                message,
            } => {
                assert_eq!(block, "check");
                assert!(
                    expression.contains("output.status.deep.field"),
                    "unexpected expression: {expression}"
                );
                assert!(!message.is_empty(), "guard error message must be non-empty");
            }
            other => panic!("expected MechError::GuardEvaluationError, got {other:?}"),
        }
    }

    // ---- Call block in imperative flow ----

    const CALL_IN_FLOW: &str = r#"
functions:
  f:
    input: { type: object }
    blocks:
      step1:
        prompt: "prompt step"
        schema:
          type: object
          required: [data]
          properties: { data: { type: string } }
        transitions:
          - goto: step2
      step2:
        call: helper
        input:
          val: "{{input.x}}"
  helper:
    input: { type: object }
    blocks:
      b:
        prompt: "stub"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
"#;

    #[test]
    fn call_block_in_imperative_flow() {
        let wf = load(CALL_IN_FLOW);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![json!({ "data": "from prompt" })]);
        let mut responses = BTreeMap::new();
        responses.insert("helper".into(), json!({ "result": "from call" }));
        let func_exec = FakeFuncExecutor::new(responses);
        let mut ctx = new_ctx(json!({ "x": "test" }), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &func_exec,
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        // step2 (call block) is terminal, returns call result.
        assert_eq!(out, json!({ "result": "from call" }));
    }

    // ---- Conversation management tests ------------------------------------

    // Two sequential prompt blocks share conversation history.
    #[test]
    fn sequential_prompt_blocks_share_conversation_history() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "first prompt"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - goto: b
      b:
        prompt: "second prompt"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();

        // Capture all requests to verify history.
        let agent = CapturingAgent::new(vec![json!({ "val": "A" }), json!({ "result": "B" })]);
        let all_requests = agent.requests.clone();

        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());
        let mut conversation = Conversation::new(None);

        run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut conversation,
            None,
        ))
        .unwrap();

        let requests = all_requests.lock().unwrap();
        // First prompt block should have empty history.
        assert!(
            requests[0].history.is_empty(),
            "first block should have empty history"
        );
        // Second prompt block should have history from first block
        // (user + assistant messages synthesized by execute_prompt_block).
        assert!(
            requests[1].history.len() >= 2,
            "second block should see history from first block, got {} messages",
            requests[1].history.len()
        );

        // Conversation should have all messages accumulated.
        assert!(
            conversation.len() >= 4,
            "conversation should have 4+ messages (user+assistant x2), got {}",
            conversation.len()
        );
    }

    // History includes tool calls and tool results from agent loop.
    #[test]
    fn history_includes_tool_calls_and_results() {
        use crate::conversation::{Message, Role};

        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "use tools"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - goto: b
      b:
        prompt: "after tools"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();

        // Agent that returns tool call/result messages.
        let all_requests: std::sync::Arc<Mutex<Vec<AgentRequest>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        let reqs = all_requests.clone();
        struct ToolAgent {
            call_count: Mutex<usize>,
            requests: std::sync::Arc<Mutex<Vec<AgentRequest>>>,
        }
        impl AgentExecutor for ToolAgent {
            fn run<'a>(
                &'a self,
                request: AgentRequest,
            ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
                self.requests.lock().unwrap().push(request.clone());
                let mut count = self.call_count.lock().unwrap();
                let n = *count;
                *count += 1;
                Box::pin(async move {
                    if n == 0 {
                        // First call: return messages with tool calls.
                        Ok(AgentResponse {
                            output: serde_json::json!({ "val": "tool_result" }),
                            messages: vec![
                                Message::user(request.prompt),
                                Message::tool_call("search(query)"),
                                Message::tool_result("search result data"),
                                Message::assistant("{\"val\": \"tool_result\"}"),
                            ],
                        })
                    } else {
                        Ok(AgentResponse {
                            output: serde_json::json!({ "result": "done" }),
                            messages: vec![],
                        })
                    }
                })
            }
        }
        let agent = ToolAgent {
            call_count: Mutex::new(0),
            requests: reqs,
        };

        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());
        let mut conversation = Conversation::new(None);

        run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut conversation,
            None,
        ))
        .unwrap();

        // After first block: 4 messages (user, tool_call, tool_result, assistant).
        // Second block should see those in its history.
        let requests = all_requests.lock().unwrap();
        assert_eq!(
            requests[1].history.len(),
            4,
            "second block should see 4 messages from first block (incl tool calls)"
        );
        assert_eq!(requests[1].history[1].role, Role::ToolCall);
        assert_eq!(requests[1].history[2].role, Role::ToolResult);

        // Total conversation: 4 (from first block) + 2 (from second block, synthesized).
        assert_eq!(conversation.len(), 6);
    }

    // Self-loop accumulates conversation history across iterations.
    #[test]
    fn self_loop_accumulates_conversation_history() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      attempts: { type: integer, initial: 0 }
    blocks:
      draft:
        prompt: "draft attempt"
        schema:
          type: object
          required: [quality]
          properties: { quality: { type: number } }
        set_context:
          attempts: "context.attempts + 1"
        transitions:
          - when: 'output.quality >= 0.8'
            goto: done
          - when: 'context.attempts < 3'
            goto: draft
          - goto: done
      done:
        prompt: "finalize"
        schema:
          type: object
          required: [ok]
          properties: { ok: { type: boolean } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();

        let agent = CapturingAgent::new(vec![
            json!({ "quality": 0.3 }), // attempt 1
            json!({ "quality": 0.5 }), // attempt 2
            json!({ "quality": 0.9 }), // attempt 3 → goes to done
            json!({ "ok": true }),     // done
        ]);
        let captured = agent.requests.clone();

        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("attempts".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());
        let mut conversation = Conversation::new(None);

        run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut conversation,
            None,
        ))
        .unwrap();

        let requests = captured.lock().unwrap();
        // First draft attempt: empty history.
        assert_eq!(requests[0].history.len(), 0);
        // Second draft attempt: 2 messages from first attempt.
        assert_eq!(requests[1].history.len(), 2);
        // Third draft attempt: 4 messages from first two attempts.
        assert_eq!(requests[2].history.len(), 4);
        // Done block: 6 messages from three draft attempts.
        assert_eq!(requests[3].history.len(), 6);
    }

    // Compaction hook invoked at threshold (schedule-level).
    #[test]
    fn compaction_hook_invoked_at_threshold() {
        use crate::conversation::ResolvedCompaction;

        let yaml = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
      required: [val]
      properties: { val: { type: string } }
    context:
      rounds: { type: integer, initial: 0 }
    blocks:
      step:
        prompt: "round {{context.rounds}}"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        set_context:
          rounds: "context.rounds + 1"
        transitions:
          - when: 'context.rounds < 5'
            goto: step
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "val": "r0" }),
            json!({ "val": "r1" }),
            json!({ "val": "r2" }),
            json!({ "val": "r3" }),
            json!({ "val": "r4" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("rounds".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        // Low threshold: 100 tokens keep + 100 tokens reserve = 200 total.
        // At ~100 tokens/message, 3+ messages should trigger.
        let mut conversation = Conversation::new(Some(ResolvedCompaction {
            keep_recent_tokens: 100,
            reserve_tokens: 100,
            custom_fn: None,
        }));

        run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut conversation,
            None,
        ))
        .unwrap();

        // After 5 rounds: 10 messages (user+assistant per round).
        // Compaction should have been triggered multiple times.
        assert!(
            conversation.compaction_count() > 0,
            "compaction should have been triggered, got count={}",
            conversation.compaction_count()
        );
        // Messages are NOT modified (placeholder compaction).
        assert_eq!(conversation.len(), 10);
    }

    // ---- C-26: dominator-based output clearing on block transitions ----

    // (a) Linear A→B→C: the outputs of A and B (strict dominators of C)
    // must survive intact through every transition.  If the fix
    // incorrectly clears non-target outputs it would prematurely remove
    // them and `ctx.get_block_output` would error.
    #[test]
    fn linear_transition_preserves_dominator_outputs() {
        let wf = load(LINEAR); // A→B→C defined above
        let func = wf.document().functions.get("f").unwrap();
        let agent = SequentialAgent::new(vec![
            json!({ "val": "from_a" }),
            json!({ "val": "from_b" }),
            json!({ "val": "from_c" }),
        ]);
        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "val": "from_c" }));
        // All three outputs must still be present: A and B strictly dominate
        // every subsequent block and are preserved across each transition;
        // C is the terminal so nothing clears it.
        assert_eq!(
            ctx.get_block_output("a").unwrap(),
            &json!({ "val": "from_a" }),
            "a's output must survive as a strict dominator of b and c"
        );
        assert_eq!(
            ctx.get_block_output("b").unwrap(),
            &json!({ "val": "from_b" }),
            "b's output must survive as a strict dominator of c"
        );
        assert_eq!(
            ctx.get_block_output("c").unwrap(),
            &json!({ "val": "from_c" }),
            "c's output must be present (it is the terminal)"
        );
    }

    // (b) Backward edge (loop): when transitioning from a block back to an
    // earlier block, sibling outputs from the prior iteration must be cleared.
    //
    // Graph:  a → b_loop (iter 1, branch="b") → a (backward edge)
    //             a → c_done (iter 2, branch≠"b") → terminal
    //
    // b_loop ran in iteration 1 and left a recorded output.  When b_loop
    // transitions back to a (backward edge), strict_doms[a] = {} (a is the
    // entry block), so ALL outputs including b_loop's are cleared.  In
    // iteration 2 the execution goes to c_done, never re-entering b_loop.
    // At function completion b_loop must have no recorded output.
    //
    // With the old single-clear code, only a's output was cleared on the
    // backward edge, leaving b_loop's stale output present indefinitely.
    #[test]
    fn backward_edge_clears_prior_iteration_sibling_output() {
        // Block names chosen so that `a` sorts first alphabetically
        // (find_entry_block falls back to alphabetical order when all blocks
        // are targeted by some transition, which happens with a backward edge).
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      iter: { type: integer, initial: 0 }
    blocks:
      a:
        prompt: "entry"
        schema:
          type: object
          required: [branch]
          properties: { branch: { type: string } }
        set_context:
          iter: "context.iter + 1"
        transitions:
          - when: 'output.branch == "b"'
            goto: b_loop
          - goto: c_done
      b_loop:
        prompt: "loop body"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
        transitions:
          - when: 'context.iter < 2'
            goto: a
      c_done:
        prompt: "exit"
        schema:
          type: object
          required: [val]
          properties: { val: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        // Iteration 1: a outputs branch="b" → b_loop runs → backward edge to a.
        // Iteration 2: a outputs branch=¬b  → c_done runs (terminal).
        let agent = SequentialAgent::new(vec![
            json!({ "branch": "b" }),       // a, iter 1
            json!({ "val": "loop_iter1" }), // b_loop, iter 1
            json!({ "branch": "other" }),   // a, iter 2
            json!({ "val": "exit" }),       // c_done
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("iter".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "val": "exit" }));
        assert_eq!(ctx.get_context("iter"), Some(&json!(2)));

        // b_loop ran only in iteration 1.  Its output must have been cleared
        // when the backward edge (b_loop→a) fired: strict_doms[a]={} so all
        // outputs were cleared.  In iteration 2 b_loop was never re-entered,
        // so no new output was recorded.
        assert!(
            ctx.get_block_output("b_loop").is_err(),
            "b_loop's stale output from iteration 1 must be cleared on the backward edge"
        );
        // a and c_done are on the iteration-2 path and their outputs persist.
        assert!(
            ctx.get_block_output("a").is_ok(),
            "a's last output must be present"
        );
        assert!(
            ctx.get_block_output("c_done").is_ok(),
            "c_done's output must be present"
        );
    }

    // (c) Divergent branches: in a loop that alternates between two mutually
    // exclusive branches, the output from the branch taken in the first
    // iteration must not persist into the second iteration.
    //
    // Graph:  classify → path_a (iter 1) → classify (backward edge)
    //                  → path_b (iter 2) → terminal
    //
    // path_a and path_b are mutually exclusive.  After the loop body, the
    // execution switches to path_b.  At that point the transition
    // classify→path_b clears all non-strict-dominators of path_b.  Since
    // path_a is not a strict dominator of path_b, its output is removed.
    // A block that legitimately references `blocks.path_a.output.*` would
    // therefore see an absent key rather than stale data.
    #[test]
    fn divergent_branch_output_absent_after_switching_branch() {
        // Block names: "a_classify" sorts before "b_path_a" and "c_path_b"
        // so find_entry_block reliably picks a_classify as the entry.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    context:
      iter: { type: integer, initial: 0 }
    blocks:
      a_classify:
        prompt: "classify"
        schema:
          type: object
          required: [branch]
          properties: { branch: { type: string } }
        set_context:
          iter: "context.iter + 1"
        transitions:
          - when: 'output.branch == "a"'
            goto: b_path_a
          - goto: c_path_b
      b_path_a:
        prompt: "path a"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
        transitions:
          - when: 'context.iter < 2'
            goto: a_classify
      c_path_b:
        prompt: "path b"
        schema:
          type: object
          required: [result]
          properties: { result: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        // Iteration 1: classify→branch=a → b_path_a → backward edge to classify.
        // Iteration 2: classify→branch≠a → c_path_b (terminal).
        let agent = SequentialAgent::new(vec![
            json!({ "branch": "a" }),          // a_classify, iter 1
            json!({ "result": "path_a_out" }), // b_path_a, iter 1
            json!({ "branch": "b" }),          // a_classify, iter 2
            json!({ "result": "path_b_out" }), // c_path_b, iter 2
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("iter".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "result": "path_b_out" }));
        assert_eq!(ctx.get_context("iter"), Some(&json!(2)));

        // b_path_a executed in iteration 1 only.  When the backward edge
        // (b_path_a→a_classify) fired, strict_doms[a_classify]={} (a_classify
        // is the entry block and has no strict dominators), so ALL outputs were
        // cleared at that point.  b_path_a never re-executed in iteration 2,
        // so it has no output at function completion.
        assert!(
            ctx.get_block_output("b_path_a").is_err(),
            "abandoned branch b_path_a's output must not persist after switching to c_path_b"
        );
        // a_classify and c_path_b are on the final path and must be present.
        assert!(
            ctx.get_block_output("a_classify").is_ok(),
            "a_classify's last output must be present"
        );
        assert!(
            ctx.get_block_output("c_path_b").is_ok(),
            "c_path_b's output must be present (it is the terminal)"
        );
    }

    // Safeguard for the common self-loop path: a block that loops to itself
    // re-executes correctly under the new dominator-based clearing (draft's
    // output is cleared on each draft→draft transition because strict_doms[draft]
    // = {} for the entry block).  Also verifies that draft's final output is
    // preserved when transitioning draft→done: dom[done]={draft,done} so
    // strict_doms[done]={draft}, which keeps draft's output.
    //
    // Note: this test passes under both old (clear-target-only) and new
    // (dominator-based) implementations; its purpose is to confirm the new
    // implementation does not regress the standard self-loop path.  Regression
    // coverage for the fix itself is provided by tests (b) and (c) above.
    #[test]
    fn self_loop_dominator_clearing_allows_reexecution_and_preserves_terminal_inputs() {
        let wf = load(SELF_LOOP);
        let func = wf.document().functions.get("f").unwrap();
        // Three low-quality drafts, then one passing draft, then done.
        let agent = SequentialAgent::new(vec![
            json!({ "text": "d1", "quality": 0.3 }),
            json!({ "text": "d2", "quality": 0.5 }),
            json!({ "text": "d3", "quality": 0.9 }), // exits to done
            json!({ "final_text": "finished" }),
        ]);
        let mut fn_decls = BTreeMap::new();
        fn_decls.insert("attempts".into(), decl("integer", json!(0)));
        let mut ctx = new_ctx(json!({}), &fn_decls, &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            None,
        ))
        .unwrap();

        assert_eq!(out, json!({ "final_text": "finished" }));
        assert_eq!(ctx.get_context("attempts"), Some(&json!(3)));

        // `draft` strictly dominates `done` (dom[done] = {draft, done}),
        // so draft's last output must be preserved when transitioning draft→done.
        assert_eq!(
            ctx.get_block_output("draft").unwrap(),
            &json!({ "text": "d3", "quality": 0.9 }),
            "draft's last-iteration output must survive as a strict dominator of done"
        );
        // `done` is the terminal block; its output must be present.
        assert_eq!(
            ctx.get_block_output("done").unwrap(),
            &json!({ "final_text": "finished" }),
            "done's output must be present"
        );
    }

    // Symmetric to dataflow's
    // `dataflow_passes_consistent_system_to_each_block_via_request_field`:
    // pass a literal `Some("test-system")` as `rendered_system` and assert
    // every captured `AgentRequest.system` equals that literal across a
    // multi-block imperative function. Detects regressions where
    // `run_function_imperative` silently drops the parameter or fails to
    // forward it to `execute_prompt_block` on every block.
    #[test]
    fn imperative_scheduler_passes_consistent_system_to_each_block_via_request_field() {
        let yaml = r#"
functions:
  f:
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
        let func = wf.document().functions.get("f").unwrap();

        let agent = CapturingAgent::new(vec![json!({ "val": "A" }), json!({ "result": "B" })]);
        let captured = agent.requests.clone();

        let mut ctx = new_ctx(json!({}), &BTreeMap::new(), &BTreeMap::new());

        let out = run_blocking(run_function_imperative(
            &wf,
            "f",
            func,
            &mut ctx,
            &agent,
            &no_func_executor(),
            &mut Conversation::new(None),
            Some("test-system"),
        ))
        .unwrap();
        assert_eq!(out, json!({ "result": "B" }));

        let reqs = captured.lock().unwrap();
        assert_eq!(reqs.len(), 2, "expected one request per prompt block");
        assert_all_requests_have_system(&reqs, "test-system");
        // Imperative scheduler must not inject system into history. Verify
        // by asserting the exact role shape: first block sees an empty
        // history; second sees exactly the prior [User, Assistant] turn
        // synthesized by `prompt.rs` (mock returns empty `messages`).
        assert!(
            reqs[0].history.is_empty(),
            "first prompt block must start with empty history"
        );
        assert_eq!(
            reqs[1].history.len(),
            2,
            "second prompt block must see exactly the prior [User, Assistant] turn"
        );
        assert_eq!(reqs[1].history[0].role, crate::conversation::Role::User);
        assert_eq!(
            reqs[1].history[1].role,
            crate::conversation::Role::Assistant
        );
    }
}
