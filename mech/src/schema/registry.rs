//! JSON Schema registry and `$ref` resolution.
//!
//! This module is intentionally named **registry** rather than "schema" to
//! avoid colliding with the parent [`crate::schema`] module, which holds the
//! parse-only YAML grammar AST. The two senses of "schema" in mech are:
//!
//! * **YAML schema** (parent module) — the struct shapes a workflow file
//!   deserialises into.
//! * **JSON Schema** (this module) — the JSON Schema documents that describe
//!   block outputs and function inputs/outputs.
//!
//! # Responsibilities
//!
//! Per `docs/MECH_SPEC.md` §13 Deliverable 4:
//!
//! * Build a [`SchemaRegistry`] from the workflow-level `schemas:` map.
//! * Detect cycles among shared schemas (a schema referencing another via
//!   `$ref:#name` at its top level) and reject them at construction time.
//! * Compile every named schema with the `jsonschema` crate, producing a
//!   reusable [`jsonschema::Validator`] per name.
//! * Resolve any [`SchemaRef`] used inside a workflow to a [`ResolvedSchema`]
//!   — either a compiled validator (for inline / named schemas) or the
//!   [`ResolvedSchema::Infer`] marker for `output: infer`, whose actual
//!   schema is filled in later by Deliverable 6.
//! * Validate JSON values against resolved schemas, surfacing the JSON Pointer
//!   path of the first failing field.
//!
//! `infer` is accepted at parse and resolution time but is **not** a callable
//! validator: calling [`SchemaRegistry::validate`] on an `Infer` resolution
//! returns an error so callers must explicitly handle the deferred case.

use std::collections::{BTreeMap, BTreeSet};

use jsonschema::Validator;
use serde_json::Value;

use crate::error::{MechError, MechResult};
use crate::schema::{InferLiteral, JsonValue, SchemaRef};

/// Prefix that marks a `$ref` string. Both `$ref:#name` (workflow-level shared
/// schema) and `$ref:path` (external file, deferred) start with this prefix.
const REF_PREFIX: &str = "$ref:";

/// A schema reference that has been resolved against the registry.
///
/// Held by reference to keep the registry the single owner of compiled
/// validators. Inline schemas live inside an owned wrapper inside this enum.
#[derive(Debug)]
pub enum ResolvedSchema<'r> {
    /// A compiled validator borrowed from the registry (named shared schema).
    Named {
        /// The schema's registered name.
        name: String,
        /// The compiled validator.
        validator: &'r Validator,
    },
    /// A freshly compiled inline schema. Owned because inline schemas appear
    /// at arbitrary points in a workflow and are not stored in the registry.
    Inline(Box<Validator>),
    /// The literal `infer` placeholder. Inference is deferred to Deliverable
    /// 6; calling [`SchemaRegistry::validate`] on this variant errors.
    Infer,
}

impl<'r> ResolvedSchema<'r> {
    /// Borrow the underlying validator, if any.
    ///
    /// Returns `None` for [`ResolvedSchema::Infer`].
    pub fn validator(&self) -> Option<&Validator> {
        match self {
            ResolvedSchema::Named { validator, .. } => Some(validator),
            ResolvedSchema::Inline(v) => Some(v.as_ref()),
            ResolvedSchema::Infer => None,
        }
    }

    /// True if this resolution is the deferred `infer` placeholder.
    pub fn is_infer(&self) -> bool {
        matches!(self, ResolvedSchema::Infer)
    }
}

/// Compiled, cycle-checked registry of workflow-level shared JSON Schemas.
///
/// Built from the `workflow.schemas` map of a parsed [`crate::WorkflowFile`].
#[derive(Debug)]
pub struct SchemaRegistry {
    /// Compiled validators keyed by registered schema name.
    validators: BTreeMap<String, Validator>,
}

impl SchemaRegistry {
    /// Build a registry from a workflow's `schemas:` map.
    ///
    /// Performs three checks at construction time:
    ///
    /// 1. Every top-level `$ref:#name` (a schema document that *itself* is
    ///    just a reference to another shared schema) resolves.
    /// 2. The graph of such references is acyclic.
    /// 3. Every (post-resolution) JSON value compiles as a JSON Schema under
    ///    the `jsonschema` crate's default draft.
    pub fn build(schemas: &BTreeMap<String, JsonValue>) -> MechResult<Self> {
        // Resolve top-level $ref-only documents to their concrete bodies
        // (detecting cycles along the way) and compile each in a single pass.
        let mut validators = BTreeMap::new();
        for name in schemas.keys() {
            let mut chain: Vec<String> = Vec::new();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let body = follow_top_level_ref(name, schemas, &mut chain, &mut seen)?;
            let validator =
                jsonschema::validator_for(body).map_err(|e| MechError::SchemaInvalid {
                    name: name.clone(),
                    message: e.to_string(),
                })?;
            validators.insert(name.clone(), validator);
        }

        Ok(SchemaRegistry { validators })
    }

    /// Resolve a [`SchemaRef`] to a [`ResolvedSchema`].
    ///
    /// * `SchemaRef::Inline(v)` compiles `v` as a fresh JSON Schema.
    /// * `SchemaRef::Ref("$ref:#name")` looks `name` up in the registry.
    /// * `SchemaRef::Ref("$ref:path")` is reserved for external files
    ///   (Deliverable 7) and currently errors with [`MechError::SchemaRefMalformed`].
    /// * `SchemaRef::Infer(_)` returns [`ResolvedSchema::Infer`].
    pub fn resolve<'r>(&'r self, schema_ref: &SchemaRef) -> MechResult<ResolvedSchema<'r>> {
        match schema_ref {
            SchemaRef::Infer(InferLiteral::Infer) => Ok(ResolvedSchema::Infer),
            SchemaRef::Ref(raw) => {
                let name = parse_named_ref(raw)?;
                let validator =
                    self.validators
                        .get(name)
                        .ok_or_else(|| MechError::SchemaRefUnresolved {
                            name: name.to_string(),
                        })?;
                Ok(ResolvedSchema::Named {
                    name: name.to_string(),
                    validator,
                })
            }
            SchemaRef::Inline(value) => {
                let validator =
                    jsonschema::validator_for(value).map_err(|e| MechError::SchemaInvalid {
                        name: "<inline>".to_string(),
                        message: e.to_string(),
                    })?;
                Ok(ResolvedSchema::Inline(Box::new(validator)))
            }
        }
    }

    /// Validate a JSON `value` against a resolved `schema`.
    ///
    /// Returns the first validation error, with its JSON Pointer path
    /// (`""` for the root). For [`ResolvedSchema::Infer`] returns an error,
    /// since inference is deferred to Deliverable 6.
    pub fn validate(&self, schema: &ResolvedSchema<'_>, value: &Value) -> MechResult<()> {
        let validator = schema.validator().ok_or_else(|| MechError::SchemaInvalid {
            name: "<infer>".to_string(),
            message:
                "cannot validate against an unresolved `infer` schema (deferred to Deliverable 6)"
                    .to_string(),
        })?;

        if let Some(err) = validator.iter_errors(value).next() {
            return Err(MechError::SchemaValidationFailed {
                path: err.instance_path.to_string(),
                message: err.to_string(),
            });
        }
        Ok(())
    }
}

/// Parse a `$ref:#name` string and return the bare `name`.
///
/// `$ref:path` (no leading `#`) is reserved for external file references and
/// is rejected here as malformed for the purposes of Deliverable 4.
fn parse_named_ref(raw: &str) -> MechResult<&str> {
    let body = raw
        .strip_prefix(REF_PREFIX)
        .ok_or_else(|| MechError::SchemaRefMalformed {
            raw: raw.to_string(),
        })?;
    let name = body
        .strip_prefix('#')
        .ok_or_else(|| MechError::SchemaRefMalformed {
            raw: raw.to_string(),
        })?;
    if name.is_empty() {
        return Err(MechError::SchemaRefMalformed {
            raw: raw.to_string(),
        });
    }
    Ok(name)
}

/// Follow a chain of top-level `$ref:#name` documents until reaching a
/// concrete schema body, detecting cycles and missing names.
fn follow_top_level_ref<'a>(
    start: &str,
    schemas: &'a BTreeMap<String, JsonValue>,
    chain: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> MechResult<&'a JsonValue> {
    let mut current = start;
    loop {
        if !seen.insert(current.to_string()) {
            chain.push(current.to_string());
            return Err(MechError::SchemaRefCircular {
                chain: std::mem::take(chain),
            });
        }
        chain.push(current.to_string());

        let body = schemas
            .get(current)
            .ok_or_else(|| MechError::SchemaRefUnresolved {
                name: current.to_string(),
            })?;

        // Detect a "this document is just a $ref string" workflow-level
        // schema. We accept either:
        //   * `{"$ref": "#name"}` (standard JSON Schema pointer form, but
        //     interpreted as our workflow-level alias for ergonomics), or
        //   * a raw string `"$ref:#name"` (matches our SchemaRef::Ref form).
        if let Some(next) = top_level_ref_target(body) {
            current = next;
            continue;
        }
        return Ok(body);
    }
}

/// If `body` is a workflow-level alias for another shared schema, return the
/// target name. Otherwise return `None`.
///
/// Recognised forms:
/// * JSON string `"$ref:#name"`.
/// * JSON object `{"$ref": "#name"}` whose `$ref` value is a `#name`-style
///   pointer (no `/`), interpreted as a workflow-level alias.
fn top_level_ref_target(body: &JsonValue) -> Option<&str> {
    match body {
        Value::String(s) => s
            .strip_prefix(REF_PREFIX)
            .and_then(|rest| rest.strip_prefix('#'))
            .filter(|n| !n.is_empty()),
        Value::Object(map) if map.len() == 1 => {
            let r = map.get("$ref")?.as_str()?;
            // Only treat plain `#name` (no slash) as a workflow alias; any
            // `#/...` JSON Pointer is left to jsonschema itself.
            r.strip_prefix('#')
                .filter(|n| !n.is_empty() && !n.contains('/'))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{InferLiteral, SchemaRef};
    use serde_json::json;

    fn schemas(pairs: &[(&str, JsonValue)]) -> BTreeMap<String, JsonValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn resolves_named_ref_to_inline_schema() {
        let s = schemas(&[(
            "person",
            json!({
                "type": "object",
                "required": ["name"],
                "properties": { "name": { "type": "string" } }
            }),
        )]);
        let reg = SchemaRegistry::build(&s).expect("registry must build");

        let resolved = reg
            .resolve(&SchemaRef::Ref("$ref:#person".to_string()))
            .expect("ref must resolve");
        match &resolved {
            ResolvedSchema::Named { name, .. } => assert_eq!(name, "person"),
            _ => panic!("expected Named resolution"),
        }
        // And it can validate something.
        reg.validate(&resolved, &json!({ "name": "Ada" }))
            .expect("valid value passes");
    }

    #[test]
    fn unresolved_named_ref_errors_with_name() {
        let s = schemas(&[("person", json!({ "type": "object" }))]);
        let reg = SchemaRegistry::build(&s).unwrap();
        let err = reg
            .resolve(&SchemaRef::Ref("$ref:#missing".to_string()))
            .expect_err("missing ref must error");
        match err {
            MechError::SchemaRefUnresolved { name } => assert_eq!(name, "missing"),
            other => panic!("expected SchemaRefUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn validate_passes_and_fails() {
        let s = schemas(&[(
            "person",
            json!({
                "type": "object",
                "required": ["name", "age"],
                "properties": {
                    "name": { "type": "string" },
                    "age": { "type": "integer", "minimum": 0 }
                }
            }),
        )]);
        let reg = SchemaRegistry::build(&s).unwrap();
        let resolved = reg
            .resolve(&SchemaRef::Ref("$ref:#person".to_string()))
            .unwrap();

        reg.validate(&resolved, &json!({ "name": "Ada", "age": 36 }))
            .expect("valid passes");

        let err = reg
            .validate(&resolved, &json!({ "name": "Ada", "age": -1 }))
            .expect_err("invalid age must fail");
        match err {
            MechError::SchemaValidationFailed { path, .. } => {
                assert!(
                    path.contains("age"),
                    "validation path should mention failing field, got `{path}`"
                );
            }
            other => panic!("expected SchemaValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn validation_error_includes_json_path_to_failing_field() {
        let s = schemas(&[(
            "wrap",
            json!({
                "type": "object",
                "required": ["inner"],
                "properties": {
                    "inner": {
                        "type": "object",
                        "required": ["v"],
                        "properties": { "v": { "type": "integer" } }
                    }
                }
            }),
        )]);
        let reg = SchemaRegistry::build(&s).unwrap();
        let resolved = reg
            .resolve(&SchemaRef::Ref("$ref:#wrap".to_string()))
            .unwrap();

        let err = reg
            .validate(&resolved, &json!({ "inner": { "v": "not-an-int" } }))
            .expect_err("type mismatch must fail");
        match err {
            MechError::SchemaValidationFailed { path, .. } => {
                // jsonschema renders instance paths as JSON Pointers like
                // `/inner/v`. Pin the exact substring so a regression that
                // drops the trailing segment is caught.
                assert!(path.contains("/inner/v"), "path missing `/inner/v`: {path}");
                assert!(!path.is_empty(), "path is empty");
                assert_ne!(path, "/inner", "path is only `/inner`, missing `/v`");
            }
            other => panic!("expected SchemaValidationFailed, got {other:?}"),
        }
    }

    #[test]
    fn circular_named_ref_is_detected_and_rejected() {
        // a -> b -> a (object form)
        let s = schemas(&[
            ("a", json!({ "$ref": "#b" })),
            ("b", json!({ "$ref": "#a" })),
        ]);
        let err = SchemaRegistry::build(&s).expect_err("cycle must be rejected");
        match err {
            MechError::SchemaRefCircular { chain } => {
                assert!(chain.contains(&"a".to_string()));
                assert!(chain.contains(&"b".to_string()));
            }
            other => panic!("expected SchemaRefCircular, got {other:?}"),
        }
    }

    #[test]
    fn self_referential_named_ref_is_detected() {
        let s = schemas(&[("a", json!({ "$ref": "#a" }))]);
        let err = SchemaRegistry::build(&s).expect_err("self-cycle must be rejected");
        assert!(matches!(err, MechError::SchemaRefCircular { .. }));
    }

    #[test]
    fn workflow_level_alias_string_form_resolves() {
        // `b` is just a string `$ref:#a` aliasing `a`.
        let s = schemas(&[("a", json!({ "type": "string" })), ("b", json!("$ref:#a"))]);
        let reg = SchemaRegistry::build(&s).expect("alias must resolve");
        let resolved = reg.resolve(&SchemaRef::Ref("$ref:#b".to_string())).unwrap();
        reg.validate(&resolved, &json!("hello")).unwrap();
        let err = reg
            .validate(&resolved, &json!(42))
            .expect_err("int against string schema must fail");
        assert!(matches!(err, MechError::SchemaValidationFailed { .. }));
    }

    #[test]
    fn infer_placeholder_resolves_to_deferred_marker() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).unwrap();
        let resolved = reg
            .resolve(&SchemaRef::Infer(InferLiteral::Infer))
            .expect("infer must resolve");
        assert!(resolved.is_infer());
        // And validating against it errors loudly so callers cannot pretend
        // an unresolved `infer` schema accepted a value.
        let err = reg
            .validate(&resolved, &json!({}))
            .expect_err("validate(infer) must error");
        assert!(matches!(err, MechError::SchemaInvalid { .. }));
    }

    #[test]
    fn inline_schema_resolves_and_validates() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).unwrap();
        let inline = SchemaRef::Inline(json!({
            "type": "object",
            "required": ["x"],
            "properties": { "x": { "type": "integer" } }
        }));
        let resolved = reg.resolve(&inline).unwrap();
        reg.validate(&resolved, &json!({ "x": 1 })).unwrap();
        reg.validate(&resolved, &json!({ "x": "no" }))
            .expect_err("type mismatch must fail");
    }

    #[test]
    fn malformed_ref_string_errors() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).unwrap();
        let err = reg
            .resolve(&SchemaRef::Ref("not-a-ref".to_string()))
            .expect_err("malformed");
        assert!(matches!(err, MechError::SchemaRefMalformed { .. }));

        let err = reg
            .resolve(&SchemaRef::Ref("$ref:no-hash".to_string()))
            .expect_err("malformed");
        assert!(matches!(err, MechError::SchemaRefMalformed { .. }));

        let err = reg
            .resolve(&SchemaRef::Ref("$ref:#".to_string()))
            .expect_err("malformed");
        assert!(matches!(err, MechError::SchemaRefMalformed { .. }));
    }

    #[test]
    fn invalid_shared_schema_errors_at_build() {
        let s = schemas(&[("bad", json!({ "type": 12345 }))]);
        let err = SchemaRegistry::build(&s).expect_err("invalid schema");
        match err {
            MechError::SchemaInvalid { name, .. } => assert_eq!(name, "bad"),
            other => panic!("expected SchemaInvalid, got {other:?}"),
        }
    }
}
