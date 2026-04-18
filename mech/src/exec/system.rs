//! System-prompt rendering for function entry.
//!
//! Scope: this module owns exactly one responsibility — rendering the
//! function-level system prompt (with workflow-level fallback) once per
//! function invocation, via [`render_function_system`]. It is *not* a
//! catch-all for shared rendering helpers. Other function-entry helpers
//! (agent-config cascade, prompt-template rendering, schema resolution,
//! etc.) belong in their own modules — do not add them here.

use crate::context::ExecutionContext;
use crate::error::{MechError, MechResult};
use crate::schema::FunctionDef;
use crate::workflow::Workflow;

/// Render the function's system prompt against the current execution
/// context, picking the function-level override before falling back to the
/// workflow-level default. Returns `None` when no system is configured.
///
/// The caller is responsible for invoking this once per function invocation;
/// the returned value is the single rendered system value for that
/// invocation.
pub(crate) fn render_function_system(
    workflow: &Workflow,
    function: &FunctionDef,
    ctx: &ExecutionContext,
) -> MechResult<Option<String>> {
    let system_source = function.system.as_deref().or_else(|| {
        workflow
            .document()
            .workflow
            .as_ref()
            .and_then(|w| w.system.as_deref())
    });
    match system_source {
        Some(src) => {
            let ns = ctx.namespaces();
            let tmpl = workflow
                .template(src)
                .ok_or_else(|| MechError::InternalInvariant {
                    message: format!(
                        "system template `{src}` should have been interned at load time"
                    ),
                })?;
            Ok(Some(tmpl.render(&ns)?))
        }
        None => Ok(None),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ExecutionContext, WorkflowState};
    use crate::loader::WorkflowLoader;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn load(yaml: &str) -> crate::Workflow {
        WorkflowLoader::new().load_str(yaml).expect("load")
    }

    fn new_ctx() -> ExecutionContext {
        let ws = WorkflowState::from_declarations(&BTreeMap::new()).unwrap();
        ExecutionContext::new(
            json!({ "user": "ada" }),
            json!({ "run_id": "r1" }),
            &BTreeMap::new(),
            ws,
        )
        .unwrap()
    }

    // (a) Function-level system None, workflow-level system Some — fallback works.
    #[test]
    fn falls_back_to_workflow_system_when_function_level_missing() {
        let yaml = r#"
workflow:
  system: "workflow-level for {{input.user}}"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "go"
        schema:
          type: object
          required: [v]
          properties: { v: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let ctx = new_ctx();
        let rendered = render_function_system(&wf, func, &ctx).unwrap();
        assert_eq!(rendered.as_deref(), Some("workflow-level for ada"));
    }

    // (b) Both function-level and workflow-level system Some — function-level wins.
    #[test]
    fn function_level_system_overrides_workflow_level() {
        let yaml = r#"
workflow:
  system: "workflow-level for {{input.user}}"
functions:
  f:
    input: { type: object }
    system: "function-level for {{input.user}}"
    blocks:
      a:
        prompt: "go"
        schema:
          type: object
          required: [v]
          properties: { v: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let ctx = new_ctx();
        let rendered = render_function_system(&wf, func, &ctx).unwrap();
        assert_eq!(rendered.as_deref(), Some("function-level for ada"));
    }

    // Neither defined — returns None.
    #[test]
    fn returns_none_when_no_system_configured() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      a:
        prompt: "go"
        schema:
          type: object
          required: [v]
          properties: { v: { type: string } }
"#;
        let wf = load(yaml);
        let func = wf.document().functions.get("f").unwrap();
        let ctx = new_ctx();
        let rendered = render_function_system(&wf, func, &ctx).unwrap();
        assert_eq!(rendered, None);
    }
    // The `InternalInvariant` arm fires when the function declares a
    // system template that was never interned at load time. In production
    // this is unreachable (the loader interns every template); here we
    // hand-craft a `FunctionDef` referencing a system template string that
    // does NOT appear in any loaded workflow, then pair it with a
    // `Workflow` whose intern map lacks that key, and assert the helper
    // surfaces `MechError::InternalInvariant` rather than panicking or
    // silently returning `None`.
    #[test]
    fn internal_invariant_when_system_template_not_interned() {
        // Load a workflow whose own system templates are interned, then
        // construct a `FunctionDef` referencing a DIFFERENT system string
        // that the workflow knows nothing about.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    system: "interned for f {{input.user}}"
    blocks:
      a:
        prompt: "go"
        schema:
          type: object
          required: [v]
          properties: { v: { type: string } }
"#;
        let wf = load(yaml);
        let func_real = wf.document().functions.get("f").unwrap().clone();
        let mut func = func_real;
        // Mutate the system field to a string the loader never saw.
        func.system = Some("uninterned system template {{input.user}}".to_string());

        let ctx = new_ctx();
        let err = render_function_system(&wf, &func, &ctx)
            .expect_err("uninterned system template must surface InternalInvariant");
        match err {
            MechError::InternalInvariant { message } => {
                assert!(
                    message.contains("uninterned system template"),
                    "InternalInvariant message must mention the offending template, got: {message}"
                );
                assert!(
                    message.contains("interned at load time"),
                    "InternalInvariant message must mention the load-time invariant, got: {message}"
                );
            }
            other => panic!("expected MechError::InternalInvariant, got {other:?}"),
        }
    }
}
