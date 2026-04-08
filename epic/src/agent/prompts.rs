// Prompt templates and assembly for agent system prompts.

use crate::agent::{ChildStatus, TaskContext};
use crate::config::project::VerificationStep;
use crate::task::{TaskOutcome, TaskPath};

/// A system prompt + user query pair for an agent call.
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

    let guidance_section = ctx
        .checkpoint_guidance
        .as_deref()
        .map(|g| format!("\n\n## Checkpoint Guidance\n{g}"))
        .unwrap_or_default();

    let children_section = if ctx.children.is_empty() {
        String::new()
    } else {
        let items = ctx
            .children
            .iter()
            .map(|c| {
                let status_label = match &c.status {
                    ChildStatus::Completed => "COMPLETED".to_string(),
                    ChildStatus::Failed { reason } => format!("FAILED: {reason}"),
                    ChildStatus::Pending => "PENDING".to_string(),
                    ChildStatus::InProgress => "IN-PROGRESS".to_string(),
                };
                let disc = if c.discoveries.is_empty() {
                    String::new()
                } else {
                    format!(" | Discoveries: {}", c.discoveries.join(", "))
                };
                format!("- [{}] {}{disc}", status_label, c.goal)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\n## Child Subtasks\n{items}")
    };

    format!(
        "## Task\nGoal: {goal}\nVerification criteria:\n- {criteria}\n\n\
         ## Position\nDepth: {depth}\nParent goal: {parent}\nAncestor chain:\n{ancestors}\n\n\
         ## Siblings\nCompleted:\n{completed}\nPending:\n{pending}{guidance_section}{rationale_section}{children_section}",
        goal = ctx.task.goal,
        depth = ctx.task.depth,
        parent = parent_line,
        rationale_section = ctx
            .parent_decomposition_rationale
            .as_deref()
            .map(|r| format!("\n\n## Parent Decomposition Rationale\n{r}"))
            .unwrap_or_default(),
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
- The root task (depth 0, no parent) is always assessed as branch — never leaf.
- When unsure between leaf and branch, prefer branch. Decomposition is safer — a subtask can always be assessed as a leaf.
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
- Work within the scope of this single task. Do not refactor unrelated code or expand scope beyond the stated goal.
- The ResearchQuery tool is available to query accumulated project knowledge, prior discoveries, and design decisions. Use it when you need context about the project. Optional scope parameter: 'vault' (stored knowledge only) or 'project' (default, vault + codebase exploration to fill gaps).

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
- Each subtask should represent the minimum scope needed. Avoid over-decomposing into too many subtasks.
- Explore the codebase with tools before decomposing to understand the current state.
- The ResearchQuery tool is available to query accumulated project knowledge, prior discoveries, and design decisions. Use it when you need context about the project. Optional scope parameter: 'vault' (stored knowledge only) or 'project' (default, vault + codebase exploration to fill gaps).

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
- Focus narrowly on fixing the specific verification failure. Do not expand scope.
- The ResearchQuery tool is available to query accumulated project knowledge, prior discoveries, and design decisions. Use it when you need context about the project. Optional scope parameter: 'vault' (stored knowledge only) or 'project' (default, vault + codebase exploration to fill gaps).

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

pub fn build_design_fix_subtasks(
    ctx: &TaskContext,
    verification_issues: &str,
    round: u32,
) -> PromptPair {
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
- The ResearchQuery tool is available to query accumulated project knowledge, prior discoveries, and design decisions. Use it when you need context about the project. Optional scope parameter: 'vault' (stored knowledge only) or 'project' (default, vault + codebase exploration to fill gaps).

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

pub fn build_verify(ctx: &TaskContext, verification_steps: &[VerificationStep]) -> PromptPair {
    let steps_section = if verification_steps.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = verification_steps
            .iter()
            .map(|s| format!("- {}: `{}`", s.name, s.command.join(" ")))
            .collect();
        format!(
            "\n\nProject verification commands (from epic.toml):\n{}",
            lines.join("\n")
        )
    };

    let path_guidance = match ctx.task.path {
        Some(TaskPath::Leaf) => {
            "\n\nThis is a leaf task. Verify that the code changes are correct and complete. \
Check that the implementation matches the verification criteria. Run verification commands if available."
        }
        // Branch verification uses dedicated three-phase prompts; this path is leaf-only.
        Some(TaskPath::Branch) | None => "",
    };

    let system_prompt = format!(
        "You are a task verifier in a recursive problem-solving system.\n\
         \n\
         Independently verify whether a completed task meets its verification criteria.\n\
         Check the actual state of the codebase, not just the executor's claims.\n\
         \n\
         Guidelines:\n\
         - Read relevant files and run verification commands.\n\
         - Check each verification criterion independently.\n\
         - Report pass only if ALL criteria are met.\n\
         - Provide detailed explanation of what was checked and findings.{steps_section}\
{path_guidance}\n\
         \n\
         Respond with the required JSON schema."
    );

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

pub fn build_branch_correctness_review(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a correctness reviewer in a recursive problem-solving system.

Verify that the aggregate result of all subtasks satisfies the parent task's objectives. \
Go beyond checking that sub-problems were completed — verify that the combined output is correct as a whole.

Check:
- Interface compatibility between sibling subtasks.
- Unhandled interactions between pieces.
- Requirements that fell through decomposition gaps.
- Review committed code against the original problem statement.

Report pass only if the aggregate result is correct. Provide detailed explanation of what was checked and findings.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Review the aggregate result of all subtasks for correctness against the parent task's objectives.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_branch_completeness_review(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a completeness reviewer in a recursive problem-solving system.

Verify that the decomposition covered the entire problem. The decomposition assumed \
A + B + C = whole problem — check whether that equation holds.

Check:
- Are there gaps or requirements that no subtask addressed?
- Did the decomposition actually equal the whole problem?
- Were any edge cases or requirements lost during decomposition?

Report pass only if the decomposition fully covered the problem. Provide detailed explanation of what was checked and findings.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Review whether the decomposition into subtasks fully covered the parent task's requirements.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_branch_simplification_review(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a simplification reviewer in a recursive problem-solving system.

Check whether the decomposition introduced unnecessary complexity. When each sub-problem is solved \
independently, the aggregate may have issues only visible at the whole-problem level.

Check:
- Redundancies across subtask outputs.
- Unnecessary abstractions.
- Over-engineering.
- Missed opportunities for simpler solutions visible only at the aggregate level.
- Cross-file and cross-subtask simplification that individual leaf tasks cannot see.

Report pass if the aggregate is acceptably simple. Report fail if there are concrete simplification opportunities. \
Provide detailed explanation of what was checked and findings.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Review the aggregate subtask outputs for unnecessary complexity or simplification opportunities.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_file_level_review(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a file-level reviewer in a recursive problem-solving system.

A task has been implemented and passed build/lint/test verification. Your job is to
independently verify that the actual source file changes correctly implement the
task's goal and verification criteria.

This is semantic review — you are checking intent and correctness, not just
compilation or test passage.

Guidelines:
- Read the actual modified files to verify changes.
- Check that every verification criterion is addressed in the implementation.
- Look for: missing changes, incorrect implementations, logic errors, unintended side effects.
- Report pass only if the implementation correctly fulfills all criteria.
- Provide detailed explanation of what was reviewed and any issues found.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Review the actual source files changed by this task. Verify that the \
         implementation correctly fulfills the goal and all verification criteria.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_leaf_simplification_review(ctx: &TaskContext) -> PromptPair {
    let system_prompt = "\
You are a simplification reviewer in a recursive problem-solving system.

A leaf task has been implemented and passed both build/lint/test verification and file-level \
review. Your job is to check whether the changes created local simplification opportunities.

The leaf task has the best context for this: it just wrote the code and can spot now-redundant \
code, duplication of nearby functionality, or cleanup opportunities within the files it touched. \
This is the boy scout principle — leave the code in an equal or better state.

Check:
- Redundant or duplicated code within the changed files.
- Over-engineered solutions where simpler approaches would work.
- Cleanup opportunities in the surrounding code (not just what was written).
- Missed refactoring opportunities visible only to code that just integrated with this section.
- Code style, naming, or structural simplifications that would improve clarity.

Report pass if no concrete simplification opportunities exist, or if the code is already \
well-simplified. Report fail if there are concrete, actionable simplification opportunities. \
Provide detailed explanation of what was reviewed and any opportunities found.

Respond with the required JSON schema."
        .into();

    let query = format!(
        "{context}\n\n\
         Review the actual source files changed by this task for local simplification \
         opportunities. Check whether the changes are as simple as they can be, and whether \
         surrounding code can be simplified.",
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
         Review the child subtask status and discoveries, then decide how to proceed.",
        context = format_context(ctx),
    );

    PromptPair {
        system_prompt,
        query,
    }
}

pub fn build_design_recovery_subtasks(
    ctx: &TaskContext,
    failure_reason: &str,
    strategy: &str,
    recovery_round: u32,
) -> PromptPair {
    let system_prompt = "\
You are an Opus-level recovery agent in a recursive problem-solving system.

A child subtask has failed. You have the full context: the original goal, the decomposition \
rationale, completed subtask results, and the failure details. Your job is to design recovery \
subtasks that address the failure.

Choose one of two approaches:
- **incremental**: Preserve completed work. Create new subtasks that complement what was already \
done and handle what the failed subtask could not. Remaining pending siblings will still execute \
after your recovery subtasks.
- **full**: The decomposition strategy itself is fundamentally wrong. Create a complete new set of \
subtasks that replaces the original plan. Remaining pending siblings will be skipped.

Guidelines:
- Each recovery subtask must have a clear goal, verification criteria, and a fresh magnitude estimate.
- Recovery subtasks operate against the current code state (which includes completed siblings' changes).
- Prefer incremental when the failure is isolated. Use full only when the approach itself is wrong.
- Explore the codebase with tools to understand the current state before designing recovery subtasks.
- Aim for 1-4 recovery subtasks. Fewer is better.
- The ResearchQuery tool is available to query accumulated project knowledge, prior discoveries, and design decisions. Use it when you need context about the project. Optional scope parameter: 'vault' (stored knowledge only) or 'project' (default, vault + codebase exploration to fill gaps).

Respond with the required JSON schema."
        .into();

    let rationale_section = ctx
        .task
        .decomposition_rationale
        .as_deref()
        .map(|r| format!("\nDecomposition rationale: {r}\n"))
        .unwrap_or_default();

    let parent_discoveries_section = if ctx.parent_discoveries.is_empty() {
        String::new()
    } else {
        let items = ctx
            .parent_discoveries
            .iter()
            .map(|d| format!("- {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n## Parent Discoveries\n{items}\n")
    };

    let query = format!(
        "{context}\n\n\
         ## Recovery Context\n\
         Recovery round: {recovery_round}\n\
         Child failure reason: {failure_reason}\n\
         Recovery strategy: {strategy}\n\
{rationale_section}{parent_discoveries_section}\n\
         Design recovery subtasks. Choose incremental or full approach based on the failure nature.",
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

pub fn build_explore_for_init() -> PromptPair {
    let system_prompt = "\
You are a project analyzer. Explore the project directory to detect its build system, \
test framework, linters, and formatters.

Look for:
- Build system markers: Cargo.toml, package.json, pyproject.toml, Makefile, CMakeLists.txt, \
build.gradle, go.mod, etc.
- Test frameworks: test directories, test config files (jest.config, pytest.ini, etc.)
- Linters/formatters: clippy, eslint, ruff, black, prettier, golangci-lint, etc.
- CI config: .github/workflows/, .gitlab-ci.yml — extract build/test/lint commands as hints.

Use tools to explore the project directory. Read key config files to understand the setup.
Do NOT look in .git, node_modules, target, or other build artifact directories.

Respond with the required JSON schema."
        .into();

    let query = "Explore this project and detect its verification steps \
(build, lint, test, format commands). Read relevant config files to determine the correct commands."
        .into();

    PromptPair {
        system_prompt,
        query,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ChildSummary, SiblingSummary};
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
            checkpoint_guidance: None,
            children: Vec::new(),
            parent_discoveries: Vec::new(),
            parent_decomposition_rationale: None,
        }
    }

    #[test]
    fn prompt_builders_contain_context() {
        let ctx = test_context();
        let cases: Vec<(&str, PromptPair, &str)> = vec![
            ("assess", build_assess(&ctx), "assessor"),
            ("execute", build_execute_leaf(&ctx), "executor"),
            ("decompose", build_design_and_decompose(&ctx), "decomposer"),
            ("verify", build_verify(&ctx, &[]), "verifier"),
            (
                "file_level_review",
                build_file_level_review(&ctx),
                "file-level reviewer",
            ),
        ];
        for (name, pair, role) in &cases {
            assert!(
                pair.query.contains("implement feature X"),
                "{name}: query missing goal"
            );
            assert!(
                pair.system_prompt.contains(role),
                "{name}: system_prompt missing role '{role}'"
            );
        }
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
        assert!(pair.query.contains('2'));
        assert!(pair.system_prompt.contains("fix"));
        assert!(pair.system_prompt.contains("rewriting from scratch"));
    }

    #[test]
    fn context_format_includes_checkpoint_guidance() {
        let mut ctx = test_context();
        ctx.checkpoint_guidance = Some("Use API v2 format instead of v1".into());
        let text = format_context(&ctx);
        assert!(text.contains("## Checkpoint Guidance"));
        assert!(text.contains("Use API v2 format instead of v1"));
    }

    #[test]
    fn context_format_with_no_siblings() {
        let ctx = TaskContext {
            task: Task::new(TaskId(0), None, "root".into(), vec!["done".into()], 0),
            parent_goal: None,
            ancestor_goals: Vec::new(),
            completed_siblings: Vec::new(),
            pending_sibling_goals: Vec::new(),
            checkpoint_guidance: None,
            children: Vec::new(),
            parent_discoveries: Vec::new(),
            parent_decomposition_rationale: None,
        };
        let text = format_context(&ctx);
        assert!(text.contains("None (root task)"));
        assert!(text.contains("Completed:\nNone"));
        assert!(text.contains("Pending:\nNone"));
    }

    #[test]
    fn design_recovery_subtasks_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_design_recovery_subtasks(&ctx, "child crashed", "retry with fallback", 1);
        assert!(pair.query.contains("child crashed"));
        assert!(pair.query.contains("retry with fallback"));
        assert!(pair.query.contains('1'));
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.system_prompt.contains("recovery"));
        assert!(pair.system_prompt.contains("incremental"));
        assert!(pair.system_prompt.contains("full"));
    }

    #[test]
    fn recovery_prompt_includes_rationale_and_parent_discoveries() {
        let mut ctx = test_context();
        ctx.task.decomposition_rationale = Some("Split by module boundary for isolation".into());
        ctx.parent_discoveries = vec!["API uses v2 format".into(), "Config path changed".into()];
        let pair = build_design_recovery_subtasks(&ctx, "compile error", "incremental", 2);
        assert!(
            pair.query
                .contains("Split by module boundary for isolation"),
            "query should contain decomposition rationale"
        );
        assert!(
            pair.query.contains("## Parent Discoveries"),
            "query should contain parent discoveries header"
        );
        assert!(
            pair.query.contains("API uses v2 format"),
            "query should contain parent discovery items"
        );
        assert!(
            pair.query.contains("Config path changed"),
            "query should contain all parent discovery items"
        );
    }

    #[test]
    fn verify_prompt_contains_verification_steps() {
        let ctx = test_context();
        let steps = vec![VerificationStep {
            name: "Build".into(),
            command: vec!["cargo".into(), "build".into()],
            timeout: 300,
        }];
        let pair = build_verify(&ctx, &steps);
        assert!(
            pair.system_prompt.contains("Build: `cargo build`"),
            "system prompt should contain formatted verification step, got: {}",
            pair.system_prompt,
        );
        assert!(pair.system_prompt.contains("Project verification commands"));
    }

    #[test]
    fn design_fix_subtasks_prompt_contains_context() {
        let ctx = test_context();
        let pair = build_design_fix_subtasks(&ctx, "lint errors in module X", 2);
        assert!(pair.query.contains("lint errors in module X"));
        assert!(pair.query.contains('2'));
        assert!(pair.query.contains("implement feature X"));
        assert!(pair.system_prompt.contains("fix"));
    }

    #[test]
    fn checkpoint_prompt_with_populated_children() {
        let mut ctx = test_context();
        ctx.children = vec![
            ChildSummary {
                goal: "setup DB".into(),
                status: ChildStatus::Completed,
                discoveries: vec![],
            },
            ChildSummary {
                goal: "write migration".into(),
                status: ChildStatus::Failed {
                    reason: "syntax error".into(),
                },
                discoveries: vec![],
            },
            ChildSummary {
                goal: "add indexes".into(),
                status: ChildStatus::Pending,
                discoveries: vec![],
            },
            ChildSummary {
                goal: "run tests".into(),
                status: ChildStatus::InProgress,
                discoveries: vec![],
            },
        ];
        let pair = build_checkpoint(&ctx, &["discovered issue".into()]);
        assert!(
            pair.query.contains("## Child Subtasks"),
            "query should contain child subtasks header"
        );
        assert!(
            pair.query.contains("[COMPLETED] setup DB"),
            "query should show COMPLETED status"
        );
        assert!(
            pair.query
                .contains("[FAILED: syntax error] write migration"),
            "query should show FAILED status with reason"
        );
        assert!(
            pair.query.contains("[PENDING] add indexes"),
            "query should show PENDING status"
        );
        assert!(
            pair.query.contains("[IN-PROGRESS] run tests"),
            "query should show IN-PROGRESS status"
        );
    }

    #[test]
    fn format_context_includes_parent_decomposition_rationale() {
        let mut ctx = test_context();
        ctx.parent_decomposition_rationale = Some("Split by module boundary for isolation".into());
        let text = format_context(&ctx);
        assert!(
            text.contains("## Parent Decomposition Rationale"),
            "should contain rationale header"
        );
        assert!(
            text.contains("Split by module boundary for isolation"),
            "should contain rationale text"
        );
    }

    #[test]
    fn recovery_prompt_omits_empty_rationale_and_discoveries() {
        let mut ctx = test_context();
        ctx.task.decomposition_rationale = None;
        ctx.parent_discoveries = Vec::new();
        let pair = build_design_recovery_subtasks(&ctx, "child crashed", "retry", 1);
        assert!(
            !pair.query.contains("Decomposition rationale"),
            "should not contain rationale when None"
        );
        assert!(
            !pair.query.contains("## Parent Discoveries"),
            "should not contain parent discoveries when empty"
        );
    }

    #[test]
    fn assess_prompt_contains_root_task_and_prefer_branch() {
        let ctx = test_context();
        let pair = build_assess(&ctx);
        assert!(
            pair.system_prompt.contains("root task"),
            "assess system prompt should mention 'root task', got: {}",
            pair.system_prompt,
        );
        assert!(
            pair.system_prompt.contains("prefer branch"),
            "assess system prompt should mention 'prefer branch', got: {}",
            pair.system_prompt,
        );
    }

    #[test]
    fn scope_limiting_instructions_in_prompts() {
        let ctx = test_context();

        let execute_pair = build_execute_leaf(&ctx);
        assert!(
            execute_pair
                .system_prompt
                .contains("scope of this single task"),
            "execute_leaf system prompt should contain 'scope of this single task', got: {}",
            execute_pair.system_prompt,
        );

        let decompose_pair = build_design_and_decompose(&ctx);
        assert!(
            decompose_pair.system_prompt.contains("minimum scope"),
            "design_and_decompose system prompt should contain 'minimum scope', got: {}",
            decompose_pair.system_prompt,
        );

        let fix_pair = build_fix_leaf(&ctx, "test failed", 1);
        assert!(
            fix_pair.system_prompt.contains("Do not expand scope"),
            "fix_leaf system prompt should contain 'Do not expand scope', got: {}",
            fix_pair.system_prompt,
        );
    }

    #[test]
    fn checkpoint_prompt_failed_child_includes_reason_string() {
        let mut ctx = test_context();
        ctx.children = vec![ChildSummary {
            goal: "compile module".into(),
            status: ChildStatus::Failed {
                reason: "compile error".into(),
            },
            discoveries: vec![],
        }];
        let pair = build_checkpoint(&ctx, &["discovered issue".into()]);
        assert!(
            pair.query.contains("FAILED: compile error"),
            "checkpoint query should contain 'FAILED: compile error', got: {}",
            pair.query,
        );
    }

    #[test]
    fn verify_prompt_leaf_vs_branch_guidance() {
        let mut ctx = test_context();

        ctx.task.path = Some(TaskPath::Leaf);
        let pair = build_verify(&ctx, &[]);
        assert!(
            pair.system_prompt.contains("leaf task"),
            "leaf prompt should contain 'leaf task', got: {}",
            pair.system_prompt,
        );

        // Branch path no longer gets special guidance in build_verify
        // (branch verification uses dedicated three-phase prompts).
        ctx.task.path = Some(TaskPath::Branch);
        let pair = build_verify(&ctx, &[]);
        assert!(
            !pair.system_prompt.contains("branch task"),
            "branch path should not contain 'branch task' guidance in build_verify, got: {}",
            pair.system_prompt,
        );

        ctx.task.path = None;
        let pair = build_verify(&ctx, &[]);
        assert!(
            !pair.system_prompt.contains("leaf task")
                && !pair.system_prompt.contains("branch task"),
            "no-path prompt should not contain path-specific guidance",
        );
    }

    #[test]
    fn branch_correctness_review_prompt() {
        let ctx = test_context();
        let pair = build_branch_correctness_review(&ctx);
        assert!(
            pair.system_prompt.contains("correctness reviewer"),
            "system prompt should contain 'correctness reviewer', got: {}",
            pair.system_prompt,
        );
        assert!(
            pair.system_prompt.contains("Interface compatibility"),
            "system prompt should mention interface compatibility",
        );
        assert!(
            pair.query.contains("implement feature X"),
            "query should contain goal",
        );
    }

    #[test]
    fn branch_completeness_review_prompt() {
        let ctx = test_context();
        let pair = build_branch_completeness_review(&ctx);
        assert!(
            pair.system_prompt.contains("completeness reviewer"),
            "system prompt should contain 'completeness reviewer', got: {}",
            pair.system_prompt,
        );
        assert!(
            pair.system_prompt.contains("gaps"),
            "system prompt should mention gaps",
        );
        assert!(
            pair.query.contains("implement feature X"),
            "query should contain goal",
        );
    }

    #[test]
    fn branch_simplification_review_prompt() {
        let ctx = test_context();
        let pair = build_branch_simplification_review(&ctx);
        assert!(
            pair.system_prompt.contains("simplification reviewer"),
            "system prompt should contain 'simplification reviewer', got: {}",
            pair.system_prompt,
        );
        assert!(
            pair.system_prompt.contains("Redundancies"),
            "system prompt should mention redundancies",
        );
        assert!(
            pair.query.contains("implement feature X"),
            "query should contain goal",
        );
    }

    #[test]
    fn explore_for_init_produces_prompt_pair() {
        let pair = build_explore_for_init();
        assert!(
            pair.system_prompt.contains("project analyzer"),
            "system_prompt should contain 'project analyzer', got: {}",
            pair.system_prompt,
        );
        assert!(
            pair.query.contains("verification steps"),
            "query should mention 'verification steps', got: {}",
            pair.query,
        );
        assert!(
            pair.system_prompt.contains("Cargo.toml"),
            "system_prompt should mention common build markers like 'Cargo.toml'",
        );
    }
}
