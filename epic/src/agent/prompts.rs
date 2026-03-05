// Prompt templates and assembly for agent system prompts.

use crate::agent::TaskContext;
use crate::task::TaskOutcome;

/// A system prompt + user query pair for a Flick call.
pub struct PromptPair {
    pub system_prompt: String,
    pub query: String,
}

fn format_context(ctx: &TaskContext) -> String {
    let criteria = ctx.task.verification_criteria.join("\n- ");

    let parent_line = ctx
        .parent_goal
        .as_deref()
        .map_or_else(|| "None (root task)".into(), ToString::to_string);

    let ancestors = if ctx.ancestor_goals.is_empty() {
        "None".into()
    } else {
        ctx.ancestor_goals
            .iter()
            .enumerate()
            .map(|(i, g)| format!("{}. {g}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let completed = if ctx.completed_siblings.is_empty() {
        "None".into()
    } else {
        ctx.completed_siblings
            .iter()
            .map(|s| {
                let status = match &s.outcome {
                    TaskOutcome::Success => "SUCCESS",
                    TaskOutcome::Failed { .. } => "FAILED",
                };
                let disc = if s.discoveries.is_empty() {
                    String::new()
                } else {
                    format!(" | Discoveries: {}", s.discoveries.join(", "))
                };
                format!("- [{}] {}{disc}", status, s.goal)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let pending = if ctx.pending_sibling_goals.is_empty() {
        "None".into()
    } else {
        ctx.pending_sibling_goals
            .iter()
            .map(|g| format!("- {g}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "## Task\nGoal: {goal}\nVerification criteria:\n- {criteria}\n\n\
         ## Position\nDepth: {depth}\nParent goal: {parent}\nAncestor chain:\n{ancestors}\n\n\
         ## Siblings\nCompleted:\n{completed}\nPending:\n{pending}",
        goal = ctx.task.goal,
        depth = ctx.task.depth,
        parent = parent_line,
    )
}

pub fn build_assess(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a task assessor for a recursive problem-solving system.

Given a task, determine whether it should be executed directly (leaf) or decomposed into subtasks (branch).
Also select the appropriate model tier: haiku (simple/routine), sonnet (moderate complexity), or opus (complex/critical).

Guidelines:
- Leaf tasks should be achievable in a single focused session with tool access.
- Branch tasks are too large or complex for a single session and need decomposition.
- Use haiku for straightforward, well-defined tasks.
- Use sonnet for tasks requiring moderate reasoning or multi-step solutions.
- Use opus for tasks requiring deep analysis, complex architecture, or critical decisions.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Assess this task: determine the execution path (leaf or branch) and select the model tier.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_execute_leaf(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a task executor in a recursive problem-solving system.

Execute the given task directly using the available tools. Read files, make changes, run commands as needed.
When finished, report the outcome as success or failed.

Guidelines:
- Read relevant files before making changes.
- Make minimal, focused changes that address the task goal.
- Verify your changes compile/work before reporting success.
- Report any discoveries (unexpected findings, architectural insights) in the discoveries field.

Respond with the required JSON schema when done."
        .into();

    let query = format!(
        "{context}\n\n\
         Execute this task. Use tools to read, modify, and verify as needed.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_design_and_decompose(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a task decomposer in a recursive problem-solving system.

Given a complex task, break it down into ordered subtasks that together achieve the goal.
Each subtask must have a clear goal, verification criteria, and magnitude estimate.

Guidelines:
- Each subtask should be independently verifiable.
- Order subtasks so dependencies are resolved first.
- Use magnitude estimates: small (< 1 session), medium (1-2 sessions), large (multiple sessions / further decomposition likely).
- Aim for 2-5 subtasks. Fewer is better if the work can be cleanly divided.
- Explore the codebase with tools before decomposing to understand the current state.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Decompose this task into ordered subtasks. Use tools to explore the codebase first if needed.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_fix_leaf(ctx: &TaskContext, failure_reason: &str, attempt: u32) -> PromptPair {
    let system_prompt = "\
You are a task executor in a recursive problem-solving system.

A previous execution of this task passed but failed verification. Your job is to fix the specific \
issues identified by the verifier rather than rewriting from scratch.

Guidelines:
- Read relevant files to understand what was already done.
- Focus on fixing the specific verification failures, not rewriting everything.
- Make minimal, targeted changes that address the identified issues.
- Verify your fixes compile/work before reporting success.
- Report any discoveries (unexpected findings, architectural insights) in the discoveries field.

Respond with the required JSON schema when done."
        .into();

    let query = format!(
        "{context}\n\n\
         ## Fix Attempt\n\
         Attempt: {attempt}\n\
         Verification failure reason: {failure_reason}\n\n\
         Fix the specific issues identified above. Do not rewrite from scratch.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_design_fix_subtasks(ctx: &TaskContext, verification_issues: &str, round: u32) -> PromptPair {
    let system_prompt = "\
You are a task decomposer in a recursive problem-solving system.

A branch task's subtasks have all completed, but verification of the branch goal has failed. \
Your job is to create targeted fix subtasks that address the specific verification issues \
rather than re-decomposing the entire task from scratch.

Guidelines:
- Each fix subtask should target a specific verification failure.
- Fix subtasks should be independently verifiable.
- Use magnitude estimates: small (< 1 session), medium (1-2 sessions), large (multiple sessions / further decomposition likely).
- Keep fix subtasks minimal and focused — do not duplicate work already done by prior subtasks.
- Explore the codebase with tools before creating fix subtasks to understand the current state.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         ## Branch Verification Failure\n\
         Round: {round}\n\
         Verification issues: {verification_issues}\n\n\
         Create targeted fix subtasks to address the specific verification issues above. \
         Do not re-decompose the entire task.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_verify(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a task verifier in a recursive problem-solving system.

Independently verify whether a completed task meets its verification criteria.
Check the actual state of the codebase, not just the executor's claims.

Guidelines:
- Read relevant files and run verification commands.
- Check each verification criterion independently.
- Report pass only if ALL criteria are met.
- Provide detailed explanation of what was checked and findings.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Verify this task against its criteria. Use tools to inspect the actual state.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_checkpoint(ctx: &TaskContext, discoveries: &[String]) -> PromptPair {
    let system_prompt = "\
You are a checkpoint reviewer in a recursive problem-solving system.

After a child subtask completes and reports discoveries, decide whether to:
- proceed: continue with remaining subtasks as planned
- adjust: continue but with updated guidance for remaining subtasks
- escalate: stop and escalate to the parent because the discoveries change the approach

Respond with the required JSON schema."
        .into();

    let disc_text = discoveries
        .iter()
        .map(|d| format!("- {d}"))
        .collect::<Vec<_>>()
        .join("\n");

    let query = format!(
        "{context}\n\n\
         ## Recent Discoveries\n{disc_text}\n\n\
         Review these discoveries and decide how to proceed.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_assess_recovery(ctx: &TaskContext, failure_reason: &str) -> PromptPair {
    let system_prompt = "\
You are a recovery assessor in a recursive problem-solving system.

A child subtask has failed. Determine whether recovery is possible and suggest a strategy.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         ## Failure\nReason: {failure_reason}\n\n\
         Assess whether this failure is recoverable and suggest a strategy if so.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::SiblingSummary;
    use crate::task::{Task, TaskId};

    fn test_context() -> TaskContext {
        TaskContext {
            task: Task::new(
                TaskId(1),
                Some(TaskId(0)),
                "implement feature X".into(),
                vec!["tests pass".into(), "no clippy warnings".into()],
                2,
            ),
            parent_goal: Some("build module Y".into()),
            ancestor_goals: vec!["build module Y".into(), "root goal".into()],
            completed_siblings: vec![SiblingSummary {
                id: TaskId(2),
                goal: "setup scaffolding".into(),
                outcome: TaskOutcome::Success,
                discoveries: vec!["found existing util".into()],
            }],
            pending_sibling_goals: vec!["write docs".into()],
        }
    }

    #[test]
    fn assess_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_assess(&ctx);
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.query.contains("tests pass"));
        assert!(pair.query.contains("build module Y"));
        assert!(pair.system_prompt.contains("assessor"));
    }

    #[test]
    fn execute_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_execute_leaf(&ctx);
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.system_prompt.contains("executor"));
    }

    #[test]
    fn decompose_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_design_and_decompose(&ctx);
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.system_prompt.contains("decomposer"));
    }

    #[test]
    fn verify_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_verify(&ctx);
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.system_prompt.contains("verifier"));
    }

    #[test]
    fn checkpoint_prompt_includes_discoveries() {
        let ctx = test_context();
        let pair = build_checkpoint(&ctx, &["found a bug".into(), "API changed".into()]);
        assert!(pair.query.contains("found a bug"));
        assert!(pair.query.contains("API changed"));
        assert!(pair.system_prompt.contains("checkpoint"));
    }

    #[test]
    fn recovery_prompt_includes_failure() {
        let ctx = test_context();
        let pair = build_assess_recovery(&ctx, "compilation failed");
        assert!(pair.query.contains("compilation failed"));
        assert!(pair.system_prompt.contains("recovery"));
    }

    #[test]
    fn fix_leaf_prompt_contains_failure_context() {
        let ctx = test_context();
        let pair = build_fix_leaf(&ctx, "test X not passing", 2);
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.query.contains("test X not passing"));
        assert!(pair.query.contains("2"));
        assert!(pair.system_prompt.contains("fix"));
        assert!(pair.system_prompt.contains("rewriting from scratch"));
    }

    #[test]
    fn context_format_with_no_siblings() {
        let ctx = TaskContext {
            task: Task::new(TaskId(0), None, "root".into(), vec!["done".into()], 0),
            parent_goal: None,
            ancestor_goals: Vec::new(),
            completed_siblings: Vec::new(),
            pending_sibling_goals: Vec::new(),
        };
        let text = format_context(&ctx);
        assert!(text.contains("None (root task)"));
        assert!(text.contains("Completed:\nNone"));
        assert!(text.contains("Pending:\nNone"));
    }

    #[test]
    fn design_fix_subtasks_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_design_fix_subtasks(&ctx, "lint errors in module X", 2);
        assert!(pair.query.contains("lint errors in module X"));
        assert!(pair.query.contains("2"));
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.system_prompt.contains("fix"));
    }
}
