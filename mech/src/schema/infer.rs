//! Function output schema inference (§13 Deliverable 6, C-07 fix).
//!
//! When a function declares `output: infer` (or omits `output:` entirely, which
//! per §4.2 defaults to `infer`), the concrete output schema is derived by
//! walking the function's terminal blocks. The shape depends on execution mode:
//!
//! * **Single terminal (either mode):** output schema = that terminal's schema.
//! * **Multiple terminals, dataflow mode:** output schema is a keyed-map object
//!   `{type: object, required: [t1, t2, …], properties: {t1: s1, t2: s2, …}}`,
//!   keyed by terminal block name. This mirrors the runtime, which returns a
//!   JSON object keyed by block name when collecting multiple dataflow sinks
//!   ([`crate::exec::dataflow::run_function_dataflow`]). Callers reference
//!   `{{callee.output.sink_name.field}}`.
//! * **Multiple terminals, imperative (CFG) mode:** all terminals must share
//!   the same schema (structural equality after `$ref` resolution). If they do,
//!   that schema becomes the function output; if they differ, inference fails
//!   with [`MechError::OutputSchemaInferenceFailed`] — the author must supply
//!   an explicit `output:` schema. No `oneOf`/`anyOf` synthesis is attempted.
//!   (In CFG mode only one terminal is ever reached per execution, so the
//!   output schema is always that one terminal's schema.)
//!
//! This module runs **after** load-time validation (§13 Deliverable 5) and
//! **before** the end-to-end loader (Deliverable 7). It mutates the parsed
//! [`MechDocument`] in place, replacing every `output: infer` with a concrete
//! inline schema.
//!
//! ### Terminal output extraction
//!
//! * **Prompt blocks** — the declared `schema:` (inline or `$ref:`), resolved
//!   against the workflow's shared schemas.
//! * **Call blocks** — only supported when the block has no `output:` mapping
//!   **and** is a single-function call: the block's output is then exactly the
//!   called function's own (already concrete) output schema. Call blocks with
//!   an `output:` mapping, or list-call shapes, cannot be structurally inferred
//!   and must be reached from a function with an explicit `output:` schema.

use std::collections::BTreeMap;

use crate::error::{MechError, MechResult};
use crate::schema::registry::resolve_schema_ref_in_map;
use crate::schema::{
    BlockDef, CallSpec, FunctionDef, InferMode, JsonValue, MechDocument, SchemaRef, infer_mode,
};

/// Infer concrete output schemas for every function that declares
/// `output: infer` (or omits `output:`).
///
/// Idempotent: running twice on the same workflow produces the same result,
/// because the first pass replaces every inferred schema with
/// [`SchemaRef::Inline`], which the second pass leaves untouched.
pub fn infer_function_outputs(wf: &mut MechDocument) -> MechResult<()> {
    // Snapshot the shared schemas map so we can resolve `$ref:#name` bodies
    // without borrowing `wf` mutably and immutably at the same time.
    let shared: BTreeMap<String, JsonValue> = wf
        .workflow
        .as_ref()
        .map(|w| w.schemas.clone())
        .unwrap_or_default();

    // We need each function's *current* declared output (possibly still
    // `infer`) when a terminal call block delegates to it. Snapshot before
    // mutation; then resolve functions in an iterative fixed-point pass so
    // that a function whose terminals are call blocks to other functions can
    // pick up those callees' inferred schemas.
    let func_names: Vec<String> = wf.functions.keys().cloned().collect();

    // Fixed-point loop: keep inferring until no more progress is made. The
    // outer bound of `func_names.len() + 1` iterations guarantees termination
    // even in the worst case (one function resolved per pass).
    let max_passes = func_names.len() + 1;
    for _ in 0..max_passes {
        let mut progressed = false;
        // Snapshot current concrete outputs (inline JSON values) for cross-
        // function lookup by terminal call blocks.
        let concrete_outputs = snapshot_concrete_outputs(wf);

        for name in &func_names {
            let func = wf.functions.get(name).expect("function name from snapshot");
            if !needs_inference(&func.output) {
                continue;
            }

            let inferred = match try_infer_function(name, func, &shared, &concrete_outputs)? {
                Some(v) => v,
                None => continue, // Not yet resolvable this pass.
            };

            let func_mut = wf
                .functions
                .get_mut(name)
                .expect("function name from snapshot");
            func_mut.output = Some(SchemaRef::Inline(inferred));
            progressed = true;
        }

        if !progressed {
            break;
        }
    }

    // Any function still declaring `infer` after the fixed point is an error.
    for (name, func) in &wf.functions {
        if needs_inference(&func.output) {
            return Err(MechError::OutputSchemaInferenceFailed {
                function: name.clone(),
                message: "unable to infer function output schema: no resolvable terminal block \
                          provided a concrete schema"
                    .to_string(),
            });
        }
    }

    Ok(())
}

/// True if `output` is absent or the literal `infer`.
fn needs_inference(output: &Option<SchemaRef>) -> bool {
    matches!(output, None | Some(SchemaRef::Infer),)
}

/// Build a map of `function name -> concrete inline output schema JSON` for
/// every function whose output is currently inline (post-resolution of a
/// `$ref:#name`). Functions whose output is still `infer` are omitted.
fn snapshot_concrete_outputs(wf: &MechDocument) -> BTreeMap<String, JsonValue> {
    let mut out = BTreeMap::new();
    let shared = wf
        .workflow
        .as_ref()
        .map(|w| &w.schemas)
        .cloned()
        .unwrap_or_default();
    for (name, func) in &wf.functions {
        if let Some(s) = &func.output
            && let Some(v) = resolve_schema_ref_in_map(s, &shared)
        {
            out.insert(name.clone(), v);
        }
    }
    out
}

/// Collect the terminal block names for a function.
///
/// * If `func.terminals` is non-empty, use that explicit list.
/// * In **dataflow** mode (no transitions): terminals are blocks with no
///   transitions **and** not referenced in any other block's `depends_on`
///   (i.e. the sink nodes). This matches `exec::dataflow::find_dataflow_terminals`.
/// * In **imperative** mode: terminals are blocks with no outgoing transitions.
fn terminal_names<'a>(func: &'a FunctionDef, mode: &InferMode) -> Vec<&'a String> {
    if !func.terminals.is_empty() {
        return func.terminals.iter().collect();
    }
    match mode {
        InferMode::Dataflow => {
            // Sink detection: not depended upon by any other block.
            let depended_upon: std::collections::BTreeSet<&str> = func
                .blocks
                .values()
                .flat_map(|b| b.depends_on().iter().map(|s| s.as_str()))
                .collect();
            func.blocks
                .iter()
                .filter(|(name, block)| {
                    block.transitions().is_empty() && !depended_upon.contains(name.as_str())
                })
                .map(|(n, _)| n)
                .collect()
        }
        InferMode::Imperative => func
            .blocks
            .iter()
            .filter(|(_, b)| b.transitions().is_empty())
            .map(|(n, _)| n)
            .collect(),
    }
}

/// Attempt to infer a single function's output schema. Returns:
///
/// * `Ok(Some(schema))` on success.
/// * `Ok(None)` if the function depends on a yet-unresolved callee (caller
///   should loop and retry after more progress).
/// * `Err(..)` on a permanent error (no terminals, incompatible schemas, …).
///
/// ### Multi-terminal schema shape (Option A — keyed map)
///
/// * **Single terminal** (both modes): output schema = that terminal's schema.
/// * **Multiple terminals, dataflow mode**: output schema is an object keyed
///   by terminal block name — `{type: object, required: [t1, t2, ...],
///   properties: {t1: schema1, t2: schema2, ...}}`. This aligns with the
///   runtime, which collects all dataflow sinks into a map keyed by block
///   name (`exec::dataflow::run_function_dataflow`).
/// * **Multiple terminals, imperative (CFG) mode**: all terminals must share
///   the same schema (structural equality after `$ref` resolution). If they
///   differ, inference fails and the author must supply an explicit `output:`
///   schema. In CFG mode only one terminal is reached at runtime, so the
///   output schema is always that terminal's schema.
fn try_infer_function(
    func_name: &str,
    func: &FunctionDef,
    shared: &BTreeMap<String, JsonValue>,
    concrete_outputs: &BTreeMap<String, JsonValue>,
) -> MechResult<Option<JsonValue>> {
    let mode = infer_mode(func);
    let terminals = terminal_names(func, &mode);

    if terminals.is_empty() {
        return Err(MechError::OutputSchemaInferenceFailed {
            function: func_name.to_string(),
            message: "function has no terminal blocks; cannot infer output schema".to_string(),
        });
    }

    // Resolve every terminal block's schema.  A single Deferred aborts this
    // pass — the caller will retry after more progress.
    let mut terminal_schemas: Vec<(String, JsonValue)> = Vec::with_capacity(terminals.len());
    for t_name in terminals {
        let block = func.blocks.get(t_name).ok_or_else(|| {
            // Validate should have caught this, but be defensive.
            MechError::OutputSchemaInferenceFailed {
                function: func_name.to_string(),
                message: format!("terminal block `{t_name}` does not exist"),
            }
        })?;
        let block_schema = match terminal_block_output(block, shared, concrete_outputs) {
            TerminalOutput::Concrete(v) => v,
            TerminalOutput::Deferred => return Ok(None),
            TerminalOutput::Error(msg) => {
                return Err(MechError::OutputSchemaInferenceFailed {
                    function: func_name.to_string(),
                    message: format!("terminal block `{t_name}`: {msg}"),
                });
            }
        };
        terminal_schemas.push((t_name.clone(), block_schema));
    }

    // Single terminal: same result for both modes.
    if terminal_schemas.len() == 1 {
        return Ok(Some(terminal_schemas.remove(0).1));
    }

    match mode {
        InferMode::Dataflow => {
            // Multiple dataflow sinks → keyed-map schema.
            // Runtime collects sink outputs into a map keyed by block name;
            // the inferred schema mirrors that shape exactly.
            // Build `required` from `properties` keys after insertion so both
            // are always in sync (one source of truth for terminal names).
            let properties: serde_json::Map<String, JsonValue> =
                terminal_schemas.into_iter().collect();
            let required: Vec<JsonValue> = properties
                .keys()
                .map(|n| JsonValue::String(n.clone()))
                .collect();
            Ok(Some(serde_json::json!({
                "type": "object",
                "required": required,
                "properties": properties,
            })))
        }
        InferMode::Imperative => {
            // Multiple CFG terminals → all must share the same schema.
            // In CFG mode only one terminal is ever reached at runtime, so
            // the output is always that terminal's raw output. If terminals
            // differ, the author must supply an explicit `output:` schema.
            let (first_name, first_schema) = &terminal_schemas[0];
            for (name, schema) in &terminal_schemas[1..] {
                if schema != first_schema {
                    return Err(MechError::OutputSchemaInferenceFailed {
                        function: func_name.to_string(),
                        message: format!(
                            "terminal blocks produce incompatible output schemas: \
                             `{first_name}` vs `{name}` — declare an explicit \
                             `output:` schema when CFG terminals differ"
                        ),
                    });
                }
            }
            Ok(Some(first_schema.clone()))
        }
    }
}

/// The three possible outcomes of resolving a terminal block's output schema.
enum TerminalOutput {
    /// Concrete JSON Schema body.
    Concrete(JsonValue),
    /// Block delegates to a callee that has not yet been inferred. The caller
    /// should loop and retry.
    Deferred,
    /// Permanent error: this block's output cannot be structurally inferred.
    Error(String),
}

fn terminal_block_output(
    block: &BlockDef,
    shared: &BTreeMap<String, JsonValue>,
    concrete_outputs: &BTreeMap<String, JsonValue>,
) -> TerminalOutput {
    match block {
        BlockDef::Prompt(p) => match &p.schema {
            SchemaRef::Inline(v) => TerminalOutput::Concrete(v.clone()),
            SchemaRef::Ref(raw) => match resolve_schema_ref_in_map(&p.schema, shared) {
                Some(v) => TerminalOutput::Concrete(v),
                None => TerminalOutput::Error(format!(
                    "prompt block schema reference `{raw}` is unresolved \
                     (unknown shared schema or unsupported form)"
                )),
            },
            SchemaRef::Infer => TerminalOutput::Error(
                "prompt block schema is `infer` (not allowed on blocks)".to_string(),
            ),
        },
        BlockDef::Call(c) => {
            if c.output.is_some() {
                return TerminalOutput::Error(
                    "call block with an `output:` mapping cannot be structurally inferred; \
                     declare an explicit function `output:` schema"
                        .to_string(),
                );
            }
            match &c.call {
                CallSpec::Single(fname) => match concrete_outputs.get(fname) {
                    Some(v) => TerminalOutput::Concrete(v.clone()),
                    None => TerminalOutput::Deferred,
                },
                _ => TerminalOutput::Error(
                    "list-form call block cannot be structurally inferred; \
                     declare an explicit function `output:` schema"
                        .to_string(),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_workflow;

    fn inferred_output(wf: &MechDocument, func: &str) -> JsonValue {
        match wf.functions[func].output.as_ref().unwrap() {
            SchemaRef::Inline(v) => v.clone(),
            other => panic!("expected inline schema, got {other:?}"),
        }
    }

    #[test]
    fn single_terminal_block_sets_function_output_to_block_output() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      only:
        prompt: "hi"
        schema:
          type: object
          required: [answer]
          properties:
            answer: { type: string }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).expect("infer ok");
        let out = inferred_output(&wf, "f");
        assert_eq!(out.get("type").and_then(|t| t.as_str()), Some("object"));
        assert_eq!(
            out.pointer("/properties/answer/type")
                .and_then(|v| v.as_str()),
            Some("string")
        );
    }

    #[test]
    fn omitted_output_defaults_to_infer() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    blocks:
      only:
        prompt: "hi"
        schema: { type: object, properties: { x: { type: integer } } }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");
        assert!(out.get("properties").is_some());
    }

    #[test]
    fn multiple_terminals_with_identical_schemas_unify() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      start:
        prompt: "route"
        schema: { type: object, properties: { r: { type: string } } }
        transitions:
          - when: "output.r == 'a'"
            goto: a
          - goto: b
      a:
        prompt: "a"
        schema: { type: object, properties: { done: { type: boolean } } }
      b:
        prompt: "b"
        schema: { type: object, properties: { done: { type: boolean } } }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");
        assert!(out.pointer("/properties/done").is_some());
        assert_eq!(
            out.pointer("/properties/done/type")
                .and_then(|v| v.as_str()),
            Some("boolean")
        );
        assert!(
            out.pointer("/properties/r").is_none(),
            "non-terminal property must not leak into inferred output"
        );
    }

    #[test]
    fn incompatible_terminals_error() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      start:
        prompt: "route"
        schema: { type: object }
        transitions:
          - when: "true"
            goto: a
          - goto: b
      a:
        prompt: "a"
        schema: { type: object, properties: { x: { type: string } } }
      b:
        prompt: "b"
        schema: { type: object, properties: { y: { type: integer } } }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        match err {
            MechError::OutputSchemaInferenceFailed { function, message } => {
                assert_eq!(function, "f");
                assert!(
                    message.contains("incompatible"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected OutputSchemaInferenceFailed, got {other:?}"),
        }
    }

    #[test]
    fn ref_and_inline_interact_correctly() {
        // Two terminals: one declares its schema by $ref, the other inline,
        // but both point at the same structural schema body. Inference must
        // unify them successfully.
        let yaml = r#"
workflow:
  schemas:
    result:
      type: object
      properties:
        value: { type: integer }
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      start:
        prompt: "route"
        schema: { type: object }
        transitions:
          - when: "true"
            goto: a
          - goto: b
      a:
        prompt: "a"
        schema: "$ref:#result"
      b:
        prompt: "b"
        schema:
          type: object
          properties:
            value: { type: integer }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");
        assert_eq!(
            out.pointer("/properties/value/type")
                .and_then(|v| v.as_str()),
            Some("integer")
        );
    }

    #[test]
    fn ref_and_inline_with_different_schemas_errors() {
        // A $ref terminal and an inline terminal with DIFFERENT schemas must
        // produce an OutputSchemaInferenceFailed error, proving the $ref path is exercised.
        let yaml = r#"
workflow:
  schemas:
    result:
      type: object
      properties:
        value: { type: integer }
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      start:
        prompt: "route"
        schema: { type: object }
        transitions:
          - when: "true"
            goto: a
          - goto: b
      a:
        prompt: "a"
        schema: "$ref:#result"
      b:
        prompt: "b"
        schema:
          type: object
          properties:
            other: { type: string }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        match err {
            MechError::OutputSchemaInferenceFailed { function, message } => {
                assert_eq!(function, "f");
                assert!(
                    message.contains("incompatible"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected OutputSchemaInferenceFailed, got {other:?}"),
        }
    }

    #[test]
    fn inference_is_idempotent() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      only:
        prompt: "hi"
        schema: { type: object, properties: { z: { type: boolean } } }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let first = wf.clone();
        infer_function_outputs(&mut wf).unwrap();
        assert_eq!(first, wf, "inference must be idempotent");
    }

    #[test]
    fn terminal_call_block_delegates_to_callee_output() {
        let yaml = r#"
functions:
  callee:
    input: { type: object }
    output:
      type: object
      properties:
        k: { type: string }
    blocks:
      only:
        prompt: "hi"
        schema: { type: object, properties: { k: { type: string } } }
  caller:
    input: { type: object }
    output: infer
    blocks:
      go:
        call: callee
        input: {}
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "caller");
        assert_eq!(
            out.pointer("/properties/k/type").and_then(|v| v.as_str()),
            Some("string")
        );
    }

    #[test]
    fn terminal_call_block_inferred_callee_resolves_via_fixed_point() {
        // Both functions are infer; callee's schema is itself inferred from
        // its one terminal prompt block. caller delegates to callee via a
        // bare single-fn call block with no output mapping.
        //
        // The caller sorts alphabetically BEFORE the callee (a_caller < z_callee),
        // forcing a real second fixed-point pass: on the first pass a_caller
        // is visited before z_callee has been resolved, so it defers;
        // z_callee resolves from its prompt block; the second pass resolves
        // a_caller.
        let yaml = r#"
functions:
  z_callee:
    input: { type: object }
    output: infer
    blocks:
      only:
        prompt: "hi"
        schema: { type: object, properties: { q: { type: integer } } }
  a_caller:
    input: { type: object }
    output: infer
    blocks:
      go:
        call: z_callee
        input: {}
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "a_caller");
        assert_eq!(
            out.pointer("/properties/q/type").and_then(|v| v.as_str()),
            Some("integer")
        );
    }

    #[test]
    fn call_block_with_output_mapping_as_terminal_errors() {
        let yaml = r#"
functions:
  callee:
    input: { type: object }
    output: { type: object }
    blocks:
      only:
        prompt: "hi"
        schema: { type: object }
  caller:
    input: { type: object }
    output: infer
    blocks:
      go:
        call: callee
        input: {}
        output:
          result: "{{callee.output}}"
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        assert!(matches!(err, MechError::OutputSchemaInferenceFailed { .. }));
    }

    #[test]
    fn function_with_no_terminals_errors() {
        // Every block has an outgoing transition to another block (a cycle);
        // therefore no terminals exist. Inference must surface a clean error
        // rather than silently producing nothing.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      a:
        prompt: "a"
        schema: { type: object }
        transitions:
          - goto: b
      b:
        prompt: "b"
        schema: { type: object }
        transitions:
          - goto: a
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        assert!(matches!(err, MechError::OutputSchemaInferenceFailed { .. }));
    }

    #[test]
    fn prompt_block_with_infer_schema_errors() {
        // A terminal prompt block that declares schema: infer triggers
        // the SchemaRef::Infer arm, which is not allowed on blocks.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      only:
        prompt: "hi"
        schema: infer
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        match err {
            MechError::OutputSchemaInferenceFailed { function, message } => {
                assert_eq!(function, "f");
                assert!(
                    message.contains("not allowed on blocks"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected OutputSchemaInferenceFailed, got {other:?}"),
        }
    }

    #[test]
    fn list_form_call_block_as_terminal_errors() {
        // A terminal call block using a uniform list form cannot be
        // structurally inferred.
        let yaml = r#"
functions:
  fn_a:
    input: { type: object }
    output:
      type: object
      properties:
        x: { type: string }
    blocks:
      b:
        prompt: "stub"
        schema: { type: object, properties: { x: { type: string } } }
  fn_b:
    input: { type: object }
    output:
      type: object
      properties:
        x: { type: string }
    blocks:
      b:
        prompt: "stub"
        schema: { type: object, properties: { x: { type: string } } }
  caller:
    input: { type: object }
    output: infer
    blocks:
      go:
        call: [fn_a, fn_b]
        input:
          v: "{{input.v}}"
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        match err {
            MechError::OutputSchemaInferenceFailed { function, message } => {
                assert_eq!(function, "caller");
                assert!(
                    message.contains("list-form call block"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected OutputSchemaInferenceFailed, got {other:?}"),
        }
    }

    #[test]
    fn explicit_terminals_field_selects_declared_terminals() {
        // When a function explicitly declares terminals: [t], only block t
        // is used for inference even though block other also has no transitions.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    terminals: [t]
    blocks:
      t:
        prompt: "terminal"
        schema:
          type: object
          properties:
            chosen: { type: boolean }
      other:
        prompt: "not a terminal"
        schema:
          type: object
          properties:
            ignored: { type: string }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");
        assert!(
            out.pointer("/properties/chosen").is_some(),
            "expected terminal t's schema"
        );
        assert!(
            out.pointer("/properties/ignored").is_none(),
            "non-terminal block's schema must not leak"
        );
    }

    #[test]
    fn ref_to_unknown_shared_schema_on_terminal_prompt_errors() {
        // A terminal prompt block whose schema references a shared schema
        // that doesn't exist triggers the unresolved-ref error path.
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      only:
        prompt: "hi"
        schema: "$ref:#nonexistent"
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let err = infer_function_outputs(&mut wf).expect_err("must error");
        match err {
            MechError::OutputSchemaInferenceFailed { function, message } => {
                assert_eq!(function, "f");
                assert!(
                    message.contains("unresolved"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected OutputSchemaInferenceFailed, got {other:?}"),
        }
    }

    #[test]
    fn explicit_output_is_left_untouched() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output:
      type: object
      properties:
        hello: { type: string }
    blocks:
      only:
        prompt: "hi"
        schema: { type: object, properties: { different: { type: integer } } }
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        let before = wf.functions["f"].output.clone();
        infer_function_outputs(&mut wf).unwrap();
        assert_eq!(before, wf.functions["f"].output);
        let out = inferred_output(&wf, "f");
        assert!(out.pointer("/properties/hello").is_some());
    }

    // ---- Dataflow multi-terminal inference (Option A: keyed map) ---------

    /// A pure dataflow function (blocks connected only by `depends_on`, no
    /// transitions) with two sink blocks must infer an object schema with one
    /// property per terminal, keyed by block name. This matches the runtime
    /// behavior of `exec::dataflow::run_function_dataflow`, which collects
    /// multiple terminal outputs into `{block_name: output}` (C-07 fix).
    #[test]
    fn dataflow_multi_terminal_infers_keyed_map_schema() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      root:
        prompt: "root"
        schema: { type: object, properties: { data: { type: string } } }
      sink_a:
        prompt: "a"
        schema: { type: object, properties: { a_result: { type: string } } }
        depends_on: [root]
      sink_b:
        prompt: "b"
        schema: { type: object, properties: { b_result: { type: integer } } }
        depends_on: [root]
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");

        // Top-level shape: keyed-map object.
        assert_eq!(out.get("type").and_then(|v| v.as_str()), Some("object"));
        let required = out.get("required").and_then(|v| v.as_array()).unwrap();
        let required_strs: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_strs.contains(&"sink_a"), "sink_a must be required");
        assert!(required_strs.contains(&"sink_b"), "sink_b must be required");

        // Each terminal's schema is nested under its name.
        assert_eq!(
            out.pointer("/properties/sink_a/properties/a_result/type")
                .and_then(|v| v.as_str()),
            Some("string"),
            "sink_a schema must be nested under 'sink_a' key"
        );
        assert_eq!(
            out.pointer("/properties/sink_b/properties/b_result/type")
                .and_then(|v| v.as_str()),
            Some("integer"),
            "sink_b schema must be nested under 'sink_b' key"
        );

        // Intermediate (non-sink) block `root` must NOT appear in the schema
        // (neither in `properties` nor in `required`).
        assert!(
            out.pointer("/properties/root").is_none(),
            "intermediate dataflow block 'root' must not appear in properties"
        );
        let required_strs2: Vec<&str> = out
            .get("required")
            .and_then(|v| v.as_array())
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !required_strs2.contains(&"root"),
            "intermediate dataflow block 'root' must not appear in required"
        );
    }

    /// Dataflow functions with identical sink schemas still produce a keyed
    /// map (Option A), not the collapsed shared schema. Callers must reference
    /// `{{callee.output.sink_a.field}}`, not `{{callee.output.field}}`.
    #[test]
    fn dataflow_multi_terminal_identical_schemas_still_keyed_map() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      root:
        prompt: "root"
        schema: { type: object, properties: { data: { type: string } } }
      sink_a:
        prompt: "a"
        schema: { type: object, properties: { done: { type: boolean } } }
        depends_on: [root]
      sink_b:
        prompt: "b"
        schema: { type: object, properties: { done: { type: boolean } } }
        depends_on: [root]
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");

        // Must be keyed map, not the collapsed shared schema.
        assert_eq!(out.get("type").and_then(|v| v.as_str()), Some("object"));
        // Each sink's schema is nested under its name.
        assert!(
            out.pointer("/properties/sink_a").is_some(),
            "sink_a must appear as a key in the keyed-map schema"
        );
        assert!(
            out.pointer("/properties/sink_b").is_some(),
            "sink_b must appear as a key in the keyed-map schema"
        );
        // The shared field 'done' is NOT a top-level property.
        assert!(
            out.pointer("/properties/done").is_none(),
            "shared field must be nested under terminal key, not at top level"
        );
    }

    /// A pure dataflow function with a single sink produces the sink's schema
    /// directly (no wrapping key) — single-terminal behavior is unchanged.
    #[test]
    fn dataflow_single_terminal_returns_sink_schema_directly() {
        let yaml = r#"
functions:
  f:
    input: { type: object }
    output: infer
    blocks:
      root:
        prompt: "root"
        schema: { type: object, properties: { data: { type: string } } }
      sink:
        prompt: "sink"
        schema: { type: object, properties: { result: { type: integer } } }
        depends_on: [root]
"#;
        let mut wf = parse_workflow(yaml).unwrap();
        infer_function_outputs(&mut wf).unwrap();
        let out = inferred_output(&wf, "f");

        // Single terminal in dataflow mode: return sink's schema directly
        // (no wrapper object).
        assert_eq!(
            out.pointer("/properties/result/type")
                .and_then(|v| v.as_str()),
            Some("integer")
        );
        assert!(
            out.pointer("/properties/sink").is_none(),
            "single-terminal dataflow must not wrap schema in a keyed map"
        );
    }
}
