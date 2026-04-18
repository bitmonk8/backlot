//! JSON Schema registry: compiled validators and `$ref` resolution utilities.
//!
//! This module holds the compiled JSON Schema validators and `$ref:#name`
//! resolution logic, distinct from the parent [`crate::schema`] module which
//! defines the parse-only YAML grammar AST. The two senses of "schema" in mech are:
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
//! * Recursively resolve nested `$ref:#name` references within schema
//!   bodies (any nested JSON objects and arrays) via [`resolve_nested_refs`].
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
//! validator: calling [`ResolvedSchema::validate`] on an `Infer` resolution
//! returns an error so callers must explicitly handle the deferred case.

use std::collections::{BTreeMap, BTreeSet};

use jsonschema::Validator;
use serde_json::Value;

use crate::error::{MechError, MechResult};
use crate::schema::{JsonValue, SchemaRef};

/// Prefix that marks a `$ref` string. Both `$ref:#name` (workflow-level shared
/// schema) and `$ref:path` (external file, deferred) start with this prefix.
pub const REF_PREFIX: &str = "$ref:";

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
    /// 6; calling [`ResolvedSchema::validate`] on this variant errors.
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

    /// Validate a JSON `value` against this resolved schema.
    ///
    /// Returns the first validation error, with its JSON Pointer path
    /// (`""` for the root). For [`ResolvedSchema::Infer`] returns
    /// [`MechError::SchemaInferDeferred`].
    pub fn validate(&self, value: &Value) -> MechResult<()> {
        let validator = self.validator().ok_or(MechError::SchemaInferDeferred)?;

        if let Some(err) = validator.iter_errors(value).next() {
            return Err(MechError::SchemaValidationFailed {
                path: err.instance_path.to_string(),
                message: err.to_string(),
            });
        }
        Ok(())
    }
}

/// Compiled, cycle-checked registry of workflow-level shared JSON Schemas.
///
/// Built from the `workflow.schemas` map of a parsed [`crate::MechDocument`].
/// Also stores fully-resolved JSON bodies for each schema, accessible via
/// [`SchemaRegistry::resolved_body`].
#[derive(Debug)]
pub struct SchemaRegistry {
    /// Compiled validators keyed by registered schema name.
    validators: BTreeMap<String, Validator>,
    /// Fully resolved JSON bodies keyed by registered schema name.
    /// Stored at build time so callers can retrieve the resolved JSON
    /// without re-resolving `$ref` chains.
    resolved_bodies: BTreeMap<String, Value>,
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
        let mut resolved_bodies = BTreeMap::new();
        for name in schemas.keys() {
            let mut chain: Vec<String> = Vec::new();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let body = follow_top_level_ref(name, schemas, &mut chain, &mut seen)?;
            let resolved_body = resolve_nested_refs(body, schemas)?;
            let validator = jsonschema::validator_for(&resolved_body).map_err(|e| {
                MechError::SchemaInvalid {
                    name: name.clone(),
                    message: e.to_string(),
                }
            })?;
            resolved_bodies.insert(name.clone(), resolved_body);
            validators.insert(name.clone(), validator);
        }

        Ok(SchemaRegistry {
            validators,
            resolved_bodies,
        })
    }

    /// Resolve a [`SchemaRef`] to a [`ResolvedSchema`].
    ///
    /// * `SchemaRef::Inline(v)` compiles `v` as a fresh JSON Schema.
    /// * `SchemaRef::Ref("$ref:#name")` looks `name` up in the registry.
    /// * `SchemaRef::Ref("$ref:path")` is reserved for external files
    ///   (Deliverable 7) and currently errors with [`MechError::SchemaRefUnsupported`].
    /// * `SchemaRef::Infer` returns [`ResolvedSchema::Infer`].
    pub fn resolve<'r>(&'r self, schema_ref: &SchemaRef) -> MechResult<ResolvedSchema<'r>> {
        match schema_ref {
            SchemaRef::Infer => Ok(ResolvedSchema::Infer),
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
                let validator = jsonschema::validator_for(value).map_err(|e| {
                    MechError::InlineSchemaInvalid {
                        message: e.to_string(),
                    }
                })?;
                Ok(ResolvedSchema::Inline(Box::new(validator)))
            }
        }
    }

    /// Return the fully resolved JSON body for a named schema.
    ///
    /// The body has all `$ref:#name` references recursively expanded and
    /// top-level alias chains followed, exactly as compiled by [`Self::build`].
    pub fn resolved_body(&self, name: &str) -> Option<&Value> {
        self.resolved_bodies.get(name)
    }

    /// Expand all `{"$ref": "#name"}` references within a JSON Schema value
    /// using the registry's pre-resolved bodies.
    ///
    /// This is intended for inline schemas: the registry has no stored body
    /// for them, so callers must run the raw JSON through this method to get
    /// the same ref-expanded form that named schemas receive at build time.
    /// Without this, agents would see different schema shapes depending on
    /// whether the workflow author used an inline schema or a named one.
    ///
    /// Substituted bodies are not re-walked because `resolved_bodies` values
    /// are already free of `$ref` references (fully expanded by
    /// `resolve_nested_refs` at build time). The function does recurse into
    /// the surrounding schema structure (objects and arrays) to reach all
    /// nested `{"$ref": "#name"}` sites.
    ///
    /// Only bare `{"$ref": "#name"}` objects (a single-key map) are
    /// substituted, matching the same criterion as `resolve_nested_walk` so
    /// inline and named schemas receive identical treatment.
    ///
    /// Returns [`MechError::SchemaRefUnresolved`] if a `$ref` target is not
    /// present in the registry.
    pub fn expand_refs(&self, value: &Value) -> MechResult<Value> {
        match value {
            Value::Object(map) => {
                // Recognise `{"$ref": "#name"}` — same criterion as resolve_nested_walk.
                if map.len() == 1 {
                    if let Some(Value::String(ref_str)) = map.get("$ref") {
                        if let Some(name) = try_parse_hash_pointer(ref_str) {
                            return self.resolved_bodies.get(name).cloned().ok_or_else(|| {
                                MechError::SchemaRefUnresolved {
                                    name: name.to_string(),
                                }
                            });
                        }
                    }
                }
                // Not a hash-ref — recurse into all values.
                let mut new_map = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    new_map.insert(k.clone(), self.expand_refs(v)?);
                }
                Ok(Value::Object(new_map))
            }
            Value::Array(arr) => {
                let new_arr: Result<Vec<_>, _> = arr.iter().map(|v| self.expand_refs(v)).collect();
                Ok(Value::Array(new_arr?))
            }
            // Scalars (string, number, bool, null) pass through unchanged.
            other => Ok(other.clone()),
        }
    }
}

/// Parse a `$ref:#name` string and return the bare `name`.
///
/// `$ref:path` (no leading `#`) is reserved for external file references and
/// is rejected here as unsupported (not malformed).
///
/// # Errors
///
/// Returns exactly one of two variants:
///
/// * [`MechError::SchemaRefMalformed`] — the input is missing the `$ref:`
///   prefix, has an empty body, or has an empty name after `#`.
/// * [`MechError::SchemaRefUnsupported`] — the input has the `$ref:` prefix
///   but no leading `#` (i.e. an external file reference).
///
/// Validators in `mech/src/validate/{agents,schema_check}.rs` rely on this
/// closed contract. Adding a new error variant here requires updating those
/// `match` blocks.
pub fn parse_named_ref(raw: &str) -> MechResult<&str> {
    let body = raw
        .strip_prefix(REF_PREFIX)
        .ok_or_else(|| MechError::SchemaRefMalformed {
            raw: raw.to_string(),
        })?;
    if body.is_empty() {
        return Err(MechError::SchemaRefMalformed {
            raw: raw.to_string(),
        });
    }
    let name = body
        .strip_prefix('#')
        .ok_or_else(|| MechError::SchemaRefUnsupported {
            raw: raw.to_string(),
        })?;
    if name.is_empty() {
        return Err(MechError::SchemaRefMalformed {
            raw: raw.to_string(),
        });
    }
    Ok(name)
}

/// Parse a `$ref:#name` string and return the bare `name`, or `None` if the
/// string does not match the expected form.
pub fn try_parse_named_ref(raw: &str) -> Option<&str> {
    let body = raw.strip_prefix(REF_PREFIX)?;
    let name = body.strip_prefix('#').filter(|s| !s.is_empty())?;
    Some(name)
}

/// Parse a `#name` JSON-Schema pointer fragment and return the bare `name`.
///
/// Accepts only plain workflow-level aliases (no `/` — JSON Pointer paths
/// are left to jsonschema itself) and rejects empty names. Returns `None`
/// for anything else.
fn try_parse_hash_pointer(raw: &str) -> Option<&str> {
    raw.strip_prefix('#')
        .filter(|n| !n.is_empty() && !n.contains('/'))
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

/// Recursively resolve all nested `$ref:#name` references in a JSON Schema value,
/// replacing them with the corresponding schema body from the shared schemas map.
///
/// A `$ref` object is recognised when it is a JSON object with exactly one key
/// `"$ref"` whose string value matches `#name` (no `/`). Standard JSON Schema
/// `$ref` values containing `/` are left untouched.
///
/// Detects circular references and returns [`MechError::SchemaRefCircular`] if
/// a cycle is found. Returns [`MechError::SchemaRefUnresolved`] if a target
/// name does not exist in the schemas map.
fn resolve_nested_refs(
    schema: &JsonValue,
    schemas: &BTreeMap<String, JsonValue>,
) -> MechResult<JsonValue> {
    let mut seen = BTreeSet::new();
    let mut chain = Vec::new();
    resolve_nested_walk(schema, schemas, &mut seen, &mut chain)
}

fn resolve_nested_walk(
    schema: &JsonValue,
    schemas: &BTreeMap<String, JsonValue>,
    seen: &mut BTreeSet<String>,
    chain: &mut Vec<String>,
) -> MechResult<JsonValue> {
    match schema {
        Value::Object(map) => {
            // Check if this object is a `{"$ref": "#name"}` hash-ref.
            if map.len() == 1 {
                if let Some(Value::String(ref_str)) = map.get("$ref") {
                    if let Some(name) = ref_str
                        .strip_prefix('#')
                        .filter(|n| !n.is_empty() && !n.contains('/'))
                    {
                        // Cycle detection.
                        if !seen.insert(name.to_string()) {
                            chain.push(name.to_string());
                            return Err(MechError::SchemaRefCircular {
                                chain: chain.clone(),
                            });
                        }
                        chain.push(name.to_string());
                        let target =
                            schemas
                                .get(name)
                                .ok_or_else(|| MechError::SchemaRefUnresolved {
                                    name: name.to_string(),
                                })?;
                        // Recursively resolve the target body as well.
                        let result = resolve_nested_walk(target, schemas, seen, chain);
                        seen.remove(name);
                        chain.pop();
                        return result;
                    }
                }
            }
            // Not a hash-ref — recurse into all values.
            let mut new_map = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                new_map.insert(k.clone(), resolve_nested_walk(v, schemas, seen, chain)?);
            }
            Ok(Value::Object(new_map))
        }
        Value::Array(arr) => {
            let new_arr: Result<Vec<_>, _> = arr
                .iter()
                .map(|v| resolve_nested_walk(v, schemas, seen, chain))
                .collect();
            Ok(Value::Array(new_arr?))
        }
        // Scalars (string, number, bool, null) pass through unchanged.
        other => Ok(other.clone()),
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
        Value::String(s) => try_parse_named_ref(s),
        Value::Object(map) if map.len() == 1 => {
            let r = map.get("$ref")?.as_str()?;
            // Only treat plain `#name` (no slash) as a workflow alias; any
            // `#/...` JSON Pointer is left to jsonschema itself.
            try_parse_hash_pointer(r)
        }
        _ => None,
    }
}

/// Resolve a [`SchemaRef`] to its raw JSON Schema body, following
/// alias chains.
///
/// For `$ref:#name` references, follows top-level string-form aliases
/// (`$ref:#name` → `$ref:#other` → …) with cycle detection. Returns `None`
/// for `infer`, when a target is missing, or on a circular alias chain.
pub fn resolve_schema_value(s: &SchemaRef, schemas: &BTreeMap<String, JsonValue>) -> Option<Value> {
    match s {
        SchemaRef::Inline(v) => Some(v.clone()),
        SchemaRef::Ref(raw) => {
            let name = try_parse_named_ref(raw)?;
            let mut body = schemas.get(name)?;
            let mut visited = BTreeSet::new();
            visited.insert(name);
            // Follow top-level string-form aliases ($ref:#name).
            while let Value::String(s) = body {
                let next = try_parse_named_ref(s)?;
                if !visited.insert(next) {
                    return None; // cycle detected
                }
                body = schemas.get(next)?;
            }
            Some(body.clone())
        }

        SchemaRef::Infer => None,
    }
}

/// Resolve a [`SchemaRef`] to its raw JSON body using only a flat schemas map.
///
/// Returns `None` for `infer` or when a `$ref:#name` target is missing.
/// Only follows one hop of `$ref:#name`; deeper chains must be pre-collapsed
/// by [`SchemaRegistry::build`].
pub fn resolve_schema_ref_in_map(
    s: &SchemaRef,
    schemas: &BTreeMap<String, JsonValue>,
) -> Option<JsonValue> {
    match s {
        SchemaRef::Inline(v) => Some(v.clone()),
        SchemaRef::Ref(raw) => {
            let name = try_parse_named_ref(raw)?;
            schemas.get(name).cloned()
        }
        SchemaRef::Infer => None,
    }
}

/// Check whether a JSON value matches the given JSON Schema type name.
pub fn value_matches_json_type(v: &Value, ty: &str) -> bool {
    match ty {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SchemaRef;
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
        resolved
            .validate(&json!({ "name": "Ada" }))
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

        resolved
            .validate(&json!({ "name": "Ada", "age": 36 }))
            .expect("valid passes");

        let err = resolved
            .validate(&json!({ "name": "Ada", "age": -1 }))
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

        let err = resolved
            .validate(&json!({ "inner": { "v": "not-an-int" } }))
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
        resolved.validate(&json!("hello")).unwrap();
        let err = resolved
            .validate(&json!(42))
            .expect_err("int against string schema must fail");
        assert!(matches!(err, MechError::SchemaValidationFailed { .. }));
    }

    #[test]
    fn infer_placeholder_resolves_to_deferred_marker() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).unwrap();
        let resolved = reg.resolve(&SchemaRef::Infer).expect("infer must resolve");
        assert!(resolved.is_infer());
        // And validating against it errors loudly so callers cannot pretend
        // an unresolved `infer` schema accepted a value.
        let err = resolved
            .validate(&json!({}))
            .expect_err("validate(infer) must error");
        assert!(matches!(err, MechError::SchemaInferDeferred));
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
        resolved.validate(&json!({ "x": 1 })).unwrap();
        resolved
            .validate(&json!({ "x": "no" }))
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
            .expect_err("unsupported");
        assert!(matches!(err, MechError::SchemaRefUnsupported { .. }));

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

    #[test]
    fn invalid_inline_schema_errors_at_resolve() {
        let s = schemas(&[]);
        let registry = SchemaRegistry::build(&s).expect("empty registry builds fine");
        let bad_inline = SchemaRef::Inline(json!({ "type": 12345 }));
        let err = registry
            .resolve(&bad_inline)
            .expect_err("invalid inline schema");
        match err {
            MechError::InlineSchemaInvalid { message } => {
                assert!(!message.is_empty(), "error message must not be empty");
            }
            other => panic!("expected InlineSchemaInvalid, got {other:?}"),
        }
    }

    #[test]
    fn nested_ref_in_property_is_resolved() {
        let s = schemas(&[
            (
                "Inner",
                json!({
                    "type": "object",
                    "required": ["value"],
                    "properties": { "value": { "type": "integer", "minimum": 1 } }
                }),
            ),
            (
                "Outer",
                json!({
                    "type": "object",
                    "required": ["inner"],
                    "properties": {
                        "inner": { "$ref": "#Inner" }
                    }
                }),
            ),
        ]);
        let reg = SchemaRegistry::build(&s).expect("registry must build");
        let resolved = reg
            .resolve(&SchemaRef::Ref("$ref:#Outer".to_string()))
            .unwrap();

        // Conforming value passes.
        resolved
            .validate(&json!({ "inner": { "value": 5 } }))
            .expect("valid value must pass");

        // Non-conforming value fails on the inner schema constraint.
        let err = resolved
            .validate(&json!({ "inner": { "value": 0 } }))
            .expect_err("value below minimum must fail");
        assert!(matches!(err, MechError::SchemaValidationFailed { .. }));
    }

    #[test]
    fn nested_ref_missing_target_errors() {
        let s = schemas(&[(
            "Broken",
            json!({
                "type": "object",
                "properties": {
                    "child": { "$ref": "#Missing" }
                }
            }),
        )]);
        let err = SchemaRegistry::build(&s).expect_err("missing nested ref must error");
        match err {
            MechError::SchemaRefUnresolved { name } => assert_eq!(name, "Missing"),
            other => panic!("expected SchemaRefUnresolved, got {other:?}"),
        }
    }

    #[test]
    fn nested_ref_circular_is_detected() {
        let s = schemas(&[
            (
                "A",
                json!({
                    "type": "object",
                    "properties": {
                        "b": { "$ref": "#B" }
                    }
                }),
            ),
            (
                "B",
                json!({
                    "type": "object",
                    "properties": {
                        "a": { "$ref": "#A" }
                    }
                }),
            ),
        ]);
        let err = SchemaRegistry::build(&s).expect_err("circular nested ref must error");
        match err {
            MechError::SchemaRefCircular { chain } => {
                assert!(
                    chain.contains(&"A".to_string()),
                    "chain should contain A: {chain:?}"
                );
                assert!(
                    chain.contains(&"B".to_string()),
                    "chain should contain B: {chain:?}"
                );
            }
            other => panic!("expected SchemaRefCircular, got {other:?}"),
        }
    }

    #[test]
    fn nested_ref_diamond_does_not_false_cycle() {
        let s = schemas(&[
            (
                "Inner",
                json!({
                    "type": "object",
                    "required": ["v"],
                    "properties": { "v": { "type": "integer" } }
                }),
            ),
            (
                "Outer",
                json!({
                    "type": "object",
                    "required": ["a", "b"],
                    "properties": {
                        "a": { "$ref": "#Inner" },
                        "b": { "$ref": "#Inner" }
                    }
                }),
            ),
        ]);
        let reg = SchemaRegistry::build(&s).expect("diamond must not trigger false cycle");
        let resolved = reg
            .resolve(&SchemaRef::Ref("$ref:#Outer".to_string()))
            .unwrap();
        resolved
            .validate(&json!({ "a": { "v": 1 }, "b": { "v": 2 } }))
            .expect("conforming diamond value must pass");
        resolved
            .validate(&json!({ "a": { "v": 1 }, "b": { "v": "bad" } }))
            .expect_err("non-conforming diamond value must fail");
    }

    #[test]
    fn nested_ref_inside_allof_is_resolved() {
        let s = schemas(&[
            (
                "Name",
                json!({
                    "type": "object",
                    "required": ["name"],
                    "properties": { "name": { "type": "string" } }
                }),
            ),
            (
                "Age",
                json!({
                    "type": "object",
                    "required": ["age"],
                    "properties": { "age": { "type": "integer", "minimum": 0 } }
                }),
            ),
            (
                "Person",
                json!({
                    "allOf": [
                        { "$ref": "#Name" },
                        { "$ref": "#Age" }
                    ]
                }),
            ),
        ]);
        let reg = SchemaRegistry::build(&s).expect("allOf refs must resolve");
        let resolved = reg
            .resolve(&SchemaRef::Ref("$ref:#Person".to_string()))
            .unwrap();
        resolved
            .validate(&json!({ "name": "Ada", "age": 36 }))
            .expect("conforming allOf value must pass");
        resolved
            .validate(&json!({ "name": "Ada" }))
            .expect_err("missing age must fail allOf");
    }

    #[test]
    fn nested_ref_three_level_chain() {
        let s = schemas(&[
            (
                "C",
                json!({
                    "type": "object",
                    "required": ["val"],
                    "properties": { "val": { "type": "integer", "minimum": 1 } }
                }),
            ),
            (
                "B",
                json!({
                    "type": "object",
                    "required": ["c"],
                    "properties": { "c": { "$ref": "#C" } }
                }),
            ),
            (
                "A",
                json!({
                    "type": "object",
                    "required": ["b"],
                    "properties": { "b": { "$ref": "#B" } }
                }),
            ),
        ]);
        let reg = SchemaRegistry::build(&s).expect("3-level chain must resolve");
        let resolved = reg.resolve(&SchemaRef::Ref("$ref:#A".to_string())).unwrap();
        resolved
            .validate(&json!({ "b": { "c": { "val": 5 } } }))
            .expect("conforming 3-level value must pass");
        resolved
            .validate(&json!({ "b": { "c": { "val": 0 } } }))
            .expect_err("val below minimum must fail through 3-level chain");
    }

    #[test]
    fn parse_named_ref_valid() {
        assert_eq!(parse_named_ref("$ref:#person").unwrap(), "person");
        assert_eq!(parse_named_ref("$ref:#a_b").unwrap(), "a_b");
    }

    #[test]
    fn parse_named_ref_rejects_malformed() {
        // Missing prefix
        assert!(matches!(
            parse_named_ref("person"),
            Err(MechError::SchemaRefMalformed { .. })
        ));
        // Missing hash — valid $ref prefix but unsupported form (external file ref)
        assert!(matches!(
            parse_named_ref("$ref:person"),
            Err(MechError::SchemaRefUnsupported { .. })
        ));
        // Empty body after prefix
        assert!(matches!(
            parse_named_ref("$ref:"),
            Err(MechError::SchemaRefMalformed { .. })
        ));
        // Empty name
        assert!(matches!(
            parse_named_ref("$ref:#"),
            Err(MechError::SchemaRefMalformed { .. })
        ));
    }

    #[test]
    fn try_parse_named_ref_valid() {
        assert_eq!(try_parse_named_ref("$ref:#person"), Some("person"));
    }

    #[test]
    fn try_parse_named_ref_returns_none_for_malformed() {
        assert_eq!(try_parse_named_ref("person"), None);
        assert_eq!(try_parse_named_ref("$ref:person"), None);
        assert_eq!(try_parse_named_ref("$ref:#"), None);
        assert_eq!(try_parse_named_ref(""), None);
    }

    #[test]
    fn try_parse_hash_pointer_valid() {
        assert_eq!(try_parse_hash_pointer("#person"), Some("person"));
        assert_eq!(try_parse_hash_pointer("#a_b"), Some("a_b"));
    }

    #[test]
    fn try_parse_hash_pointer_returns_none_for_invalid() {
        assert_eq!(try_parse_hash_pointer("person"), None); // missing leading #
        assert_eq!(try_parse_hash_pointer("#"), None); // empty name
        assert_eq!(try_parse_hash_pointer("#foo/bar"), None); // contains /, reserved for JSON Pointer
        assert_eq!(try_parse_hash_pointer(""), None);
    }

    #[test]
    fn value_matches_json_type_all_types() {
        assert!(value_matches_json_type(&json!("hello"), "string"));
        assert!(value_matches_json_type(&json!(1.5), "number"));
        assert!(value_matches_json_type(&json!(42), "integer"));
        assert!(value_matches_json_type(&json!(true), "boolean"));
        assert!(value_matches_json_type(&json!([1]), "array"));
        assert!(value_matches_json_type(&json!({"a": 1}), "object"));
        assert!(value_matches_json_type(&json!(null), "null"));
        assert!(!value_matches_json_type(&json!("hello"), "integer"));
        assert!(!value_matches_json_type(&json!(42), "string"));
        assert!(!value_matches_json_type(&json!(true), "unknown_type"));
    }

    #[test]
    fn resolve_schema_value_inline() {
        let empty = BTreeMap::new();
        let schema = SchemaRef::Inline(json!({"type": "string"}));
        assert_eq!(
            resolve_schema_value(&schema, &empty),
            Some(json!({"type": "string"}))
        );
    }

    #[test]
    fn resolve_schema_value_ref_resolves() {
        let s = schemas(&[("person", json!({"type": "object"}))]);
        let schema = SchemaRef::Ref("$ref:#person".to_string());
        assert_eq!(
            resolve_schema_value(&schema, &s),
            Some(json!({"type": "object"}))
        );
    }

    #[test]
    fn resolve_schema_value_infer_returns_none() {
        let empty = BTreeMap::new();
        let schema = SchemaRef::Infer;
        assert_eq!(resolve_schema_value(&schema, &empty), None);
    }

    #[test]
    fn resolve_schema_value_follows_alias_chain() {
        let s = schemas(&[
            ("alias", json!("$ref:#real")),
            (
                "real",
                json!({"type": "object", "properties": {"x": {"type": "integer"}}}),
            ),
        ]);
        let schema = SchemaRef::Ref("$ref:#alias".to_string());
        assert_eq!(
            resolve_schema_value(&schema, &s),
            Some(json!({"type": "object", "properties": {"x": {"type": "integer"}}}))
        );
    }

    #[test]
    fn resolved_body_returns_expanded_json() {
        let s = schemas(&[
            (
                "Inner",
                json!({
                    "type": "object",
                    "required": ["v"],
                    "properties": { "v": { "type": "integer" } }
                }),
            ),
            (
                "Outer",
                json!({
                    "type": "object",
                    "required": ["inner"],
                    "properties": {
                        "inner": { "$ref": "#Inner" }
                    }
                }),
            ),
        ]);
        let reg = SchemaRegistry::build(&s).expect("registry must build");
        let body = reg.resolved_body("Outer").expect("body must exist");
        // The nested $ref:#Inner must be expanded to the actual Inner schema,
        // not left as a {"$ref": "#Inner"} object.
        let inner_prop = body
            .get("properties")
            .and_then(|p| p.get("inner"))
            .expect("inner property must exist");
        assert_eq!(
            inner_prop,
            &json!({
                "type": "object",
                "required": ["v"],
                "properties": { "v": { "type": "integer" } }
            }),
            "nested $ref must be fully expanded in resolved_body"
        );
    }

    #[test]
    fn resolve_schema_value_circular_alias_returns_none() {
        let s = schemas(&[("a", json!("$ref:#b")), ("b", json!("$ref:#a"))]);
        let schema = SchemaRef::Ref("$ref:#a".to_string());
        assert_eq!(
            resolve_schema_value(&schema, &s),
            None,
            "circular alias chain must return None"
        );
    }

    #[test]
    fn resolve_schema_ref_in_map_inline_returns_value() {
        let empty = BTreeMap::new();
        let schema = SchemaRef::Inline(json!({"type": "string"}));
        assert_eq!(
            resolve_schema_ref_in_map(&schema, &empty),
            Some(json!({"type": "string"}))
        );
    }

    #[test]
    fn resolve_schema_ref_in_map_ref_existing_returns_body() {
        let s = schemas(&[("person", json!({"type": "object"}))]);
        let schema = SchemaRef::Ref("$ref:#person".to_string());
        assert_eq!(
            resolve_schema_ref_in_map(&schema, &s),
            Some(json!({"type": "object"}))
        );
    }

    #[test]
    fn resolve_schema_ref_in_map_ref_missing_returns_none() {
        let s = schemas(&[("person", json!({"type": "object"}))]);
        let schema = SchemaRef::Ref("$ref:#missing".to_string());
        assert_eq!(resolve_schema_ref_in_map(&schema, &s), None);
    }

    #[test]
    fn resolve_schema_ref_in_map_infer_returns_none() {
        let empty = BTreeMap::new();
        let schema = SchemaRef::Infer;
        assert_eq!(resolve_schema_ref_in_map(&schema, &empty), None);
    }
    // ---- Cycle / alias topology ----

    #[test]
    fn three_node_cycle_detected() {
        let s = schemas(&[
            ("a", json!({ "$ref": "#b" })),
            ("b", json!({ "$ref": "#c" })),
            ("c", json!({ "$ref": "#a" })),
        ]);
        let err = SchemaRegistry::build(&s).expect_err("3-node cycle must be rejected");
        match err {
            MechError::SchemaRefCircular { chain } => {
                assert!(chain.contains(&"a".to_string()));
                assert!(chain.contains(&"b".to_string()));
                assert!(chain.contains(&"c".to_string()));
            }
            other => panic!("expected SchemaRefCircular, got {other:?}"),
        }
    }

    #[test]
    fn multi_hop_alias_chain_resolves() {
        let s = schemas(&[
            ("a", json!({ "type": "string" })),
            ("b", json!("$ref:#a")),
            ("c", json!("$ref:#b")),
        ]);
        let reg = SchemaRegistry::build(&s).expect("multi-hop alias must resolve");
        let resolved = reg.resolve(&SchemaRef::Ref("$ref:#c".to_string())).unwrap();
        resolved.validate(&json!("hello")).unwrap();
        resolved
            .validate(&json!(42))
            .expect_err("int against string schema must fail");
    }

    #[test]
    fn external_file_ref_rejected() {
        let s = schemas(&[("ext", json!("$ref:./foo.json"))]);
        let err = SchemaRegistry::build(&s).expect_err("external file ref must be rejected");
        // The string "$ref:./foo.json" is not a valid $ref:#name (no #), so
        // top_level_ref_target returns None. It is then treated as a literal
        // schema value and passed to jsonschema::validator_for, which rejects
        // a bare string as an invalid JSON Schema.
        assert!(matches!(err, MechError::SchemaInvalid { .. }));
    }

    #[test]
    fn string_form_cycle_detected() {
        // Using string form "$ref:#name" instead of object form {"$ref": "#name"}
        let s = schemas(&[("a", json!("$ref:#b")), ("b", json!("$ref:#a"))]);
        let err = SchemaRegistry::build(&s).expect_err("string-form cycle must be rejected");
        match err {
            MechError::SchemaRefCircular { chain } => {
                assert!(chain.contains(&"a".to_string()));
                assert!(chain.contains(&"b".to_string()));
            }
            other => panic!("expected SchemaRefCircular, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // expand_refs tests — guard issue #307 (inline vs named schema produce
    // identical expanded JSON for the agent). Originally lived in
    // exec/prompt.rs but moved here since they exercise SchemaRegistry.
    // ---------------------------------------------------------------------

    // (a) Inline with no $refs passes through unchanged.
    #[test]
    fn expand_refs_inline_no_refs_passes_through_unchanged() {
        let reg = SchemaRegistry::build(&schemas(&[])).expect("registry must build");
        let schema = json!({
            "type": "object",
            "required": ["x"],
            "properties": { "x": { "type": "integer" } }
        });
        let expanded = reg.expand_refs(&schema).expect("expand must succeed");
        assert_eq!(expanded, schema);
    }

    // (b) Inline with $ref:#name expands the reference.
    #[test]
    fn expand_refs_inline_with_ref_is_expanded() {
        let reg = SchemaRegistry::build(&schemas(&[(
            "Coord",
            json!({
                "type": "object",
                "required": ["lat", "lon"],
                "properties": {
                    "lat": { "type": "number" },
                    "lon": { "type": "number" }
                }
            }),
        )]))
        .expect("registry must build");
        let inline = json!({
            "type": "object",
            "required": ["position"],
            "properties": {
                "position": { "$ref": "#Coord" }
            }
        });
        let expanded = reg.expand_refs(&inline).expect("expand must succeed");
        let position_prop = expanded
            .get("properties")
            .and_then(|p| p.get("position"))
            .expect("position property must exist");
        assert_eq!(
            position_prop,
            &json!({
                "type": "object",
                "required": ["lat", "lon"],
                "properties": {
                    "lat": { "type": "number" },
                    "lon": { "type": "number" }
                }
            }),
            "$ref must be replaced with Coord body"
        );
    }

    // (c) Named-schema path still produces an expanded body.
    #[test]
    fn expand_refs_named_schema_resolved_body_is_expanded() {
        let reg = SchemaRegistry::build(&schemas(&[
            (
                "Inner",
                json!({
                    "type": "object",
                    "required": ["v"],
                    "properties": { "v": { "type": "integer" } }
                }),
            ),
            (
                "Outer",
                json!({
                    "type": "object",
                    "required": ["inner"],
                    "properties": { "inner": { "$ref": "#Inner" } }
                }),
            ),
        ]))
        .expect("registry must build");
        let body = reg.resolved_body("Outer").expect("body must exist");
        let inner_prop = body
            .get("properties")
            .and_then(|p| p.get("inner"))
            .expect("inner must exist");
        // The named path must already be expanded (regression guard).
        assert_eq!(
            inner_prop,
            &json!({
                "type": "object",
                "required": ["v"],
                "properties": { "v": { "type": "integer" } }
            })
        );
    }

    // (d) Expanded inline matches named form for an equivalent schema.
    #[test]
    fn expand_refs_expanded_inline_matches_named_form() {
        let reg = SchemaRegistry::build(&schemas(&[
            (
                "Item",
                json!({
                    "type": "object",
                    "required": ["id"],
                    "properties": { "id": { "type": "string" } }
                }),
            ),
            (
                "Wrapper",
                json!({
                    "type": "object",
                    "required": ["item"],
                    "properties": { "item": { "$ref": "#Item" } }
                }),
            ),
        ]))
        .expect("registry must build");

        // Named path: registry.resolved_body("Wrapper")
        let named_body = reg
            .resolved_body("Wrapper")
            .expect("named body must exist")
            .clone();

        // Inline path: same structure but written inline, using expand_refs.
        let inline_schema = json!({
            "type": "object",
            "required": ["item"],
            "properties": { "item": { "$ref": "#Item" } }
        });
        let expanded_inline = reg
            .expand_refs(&inline_schema)
            .expect("expand must succeed");

        assert_eq!(
            expanded_inline, named_body,
            "expanded inline must match named resolved body"
        );
    }

    // Error path: $ref to a non-existent schema name must error with the
    // unresolved name surfaced for diagnostics.
    #[test]
    fn expand_refs_unresolved_ref_errors() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).expect("empty registry builds fine");
        let bad_inline = json!({ "$ref": "#Missing" });
        let err = reg
            .expand_refs(&bad_inline)
            .expect_err("unresolved ref must error");
        match err {
            MechError::SchemaRefUnresolved { name } => assert_eq!(name, "Missing"),
            other => panic!("expected SchemaRefUnresolved, got {other:?}"),
        }
    }

    // `{"$ref": "#X", "description": "..."}` has sibling keys — only bare
    // single-key objects are substituted, matching `resolve_nested_walk`.
    // The schema recurser walks into the map values but does not substitute
    // the whole object, so the `$ref` key's string value is left as-is.
    #[test]
    fn expand_refs_ref_with_sibling_keys_is_not_substituted() {
        let reg = SchemaRegistry::build(&schemas(&[("Foo", json!({ "type": "string" }))]))
            .expect("registry must build");
        // Object with $ref plus a sibling key — not a bare ref, so it is
        // treated as a regular object (consistent with resolve_nested_walk).
        let inline = json!({
            "type": "object",
            "properties": {
                "x": { "$ref": "#Foo", "description": "annotated" }
            }
        });
        let expanded = reg.expand_refs(&inline).expect("expand must succeed");
        let x_prop = expanded
            .get("properties")
            .and_then(|p| p.get("x"))
            .expect("x must exist");
        // Sibling-key object is NOT substituted — stays as the original map.
        assert_eq!(
            x_prop,
            &json!({ "$ref": "#Foo", "description": "annotated" }),
            "object with sibling keys must not be substituted"
        );
    }

    // Top-level sibling-key: the entry-point guard is checked directly
    // (not only through recursive descent).
    #[test]
    fn expand_refs_top_level_sibling_key_passes_through() {
        let reg = SchemaRegistry::build(&schemas(&[("Foo", json!({ "type": "string" }))]))
            .expect("registry must build");
        // At the entry-point (top level), a two-key map must not be substituted.
        let top_level = json!({ "$ref": "#Foo", "description": "annotated" });
        let expanded = reg.expand_refs(&top_level).expect("expand must succeed");
        assert_eq!(
            expanded, top_level,
            "top-level sibling-key object must not be substituted"
        );
    }

    // Sibling-key with unregistered name: no lookup is attempted, so the
    // object passes through without error even though the name is absent.
    #[test]
    fn expand_refs_sibling_key_with_unregistered_name_passes_through() {
        // Empty registry — "Ghost" is not a registered schema.
        let reg = SchemaRegistry::build(&BTreeMap::new()).expect("empty registry builds fine");
        let schema = json!({ "$ref": "#Ghost", "description": "phantom" });
        // Must succeed: the guard skips lookup entirely for multi-key objects.
        let expanded = reg
            .expand_refs(&schema)
            .expect("sibling-key with unregistered name must not error");
        assert_eq!(
            expanded, schema,
            "sibling-key with unregistered name must pass through unchanged"
        );
    }

    // Array elements: $ref objects inside arrays (oneOf / anyOf / items) are
    // substituted by the Value::Array arm of expand_refs.
    #[test]
    fn expand_refs_array_element_ref_is_expanded() {
        let reg = SchemaRegistry::build(&schemas(&[
            (
                "Coord",
                json!({
                    "type": "object",
                    "required": ["lat", "lon"],
                    "properties": {
                        "lat": { "type": "number" },
                        "lon": { "type": "number" }
                    }
                }),
            ),
            (
                "Shape",
                json!({
                    "type": "object",
                    "required": ["kind"],
                    "properties": { "kind": { "type": "string" } }
                }),
            ),
        ]))
        .expect("registry must build");
        // oneOf with two $ref elements — both live inside an array.
        let inline = json!({ "oneOf": [{ "$ref": "#Coord" }, { "$ref": "#Shape" }] });
        let expanded = reg.expand_refs(&inline).expect("expand must succeed");
        let one_of = expanded
            .get("oneOf")
            .and_then(Value::as_array)
            .expect("oneOf array must exist");
        assert_eq!(one_of.len(), 2);
        assert_eq!(
            &one_of[0],
            &json!({
                "type": "object",
                "required": ["lat", "lon"],
                "properties": {
                    "lat": { "type": "number" },
                    "lon": { "type": "number" }
                }
            }),
            "first array element must be expanded to Coord body"
        );
        assert_eq!(
            &one_of[1],
            &json!({
                "type": "object",
                "required": ["kind"],
                "properties": { "kind": { "type": "string" } }
            }),
            "second array element must be expanded to Shape body"
        );
    }

    // Deep recursion: $ref three levels deep inside nested properties.
    #[test]
    fn expand_refs_deeply_nested_ref_is_expanded() {
        let reg = SchemaRegistry::build(&schemas(&[(
            "Leaf",
            json!({ "type": "integer", "minimum": 0 }),
        )]))
        .expect("registry must build");
        let inline = json!({
            "type": "object",
            "properties": {
                "a": {
                    "type": "object",
                    "properties": {
                        "b": {
                            "type": "object",
                            "properties": {
                                "c": { "$ref": "#Leaf" }
                            }
                        }
                    }
                }
            }
        });
        let expanded = reg.expand_refs(&inline).expect("expand must succeed");
        let c_prop = expanded
            .pointer("/properties/a/properties/b/properties/c")
            .expect("c must exist at full depth");
        assert_eq!(
            c_prop,
            &json!({ "type": "integer", "minimum": 0 }),
            "$ref three levels deep must be expanded to Leaf body"
        );
    }

    // Empty-string $ref value: does not match the #name guard, so the object
    // passes through unchanged (consistent with resolve_nested_walk).
    #[test]
    fn expand_refs_empty_string_ref_passes_through() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).expect("empty registry builds fine");
        let schema = json!({ "$ref": "" });
        let expanded = reg
            .expand_refs(&schema)
            .expect("empty-string $ref must not error");
        assert_eq!(
            expanded, schema,
            "empty-string $ref must pass through unchanged"
        );
    }

    // JSON Pointer $ref (`#/definitions/X`) must NOT be substituted: the
    // `!n.contains('/')` filter delegates these to jsonschema, matching
    // resolve_nested_walk's identical guard.
    #[test]
    fn expand_refs_json_pointer_ref_passes_through() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).expect("empty registry builds fine");
        let schema = json!({ "$ref": "#/definitions/X" });
        let expanded = reg
            .expand_refs(&schema)
            .expect("JSON Pointer $ref must not error");
        assert_eq!(
            expanded, schema,
            "JSON Pointer $ref must pass through unchanged (slash-form delegated to jsonschema)"
        );
    }

    // $ref value with no `#` prefix (e.g. external file ref) also passes
    // through unchanged — the strip_prefix('#') guard rejects it before any
    // registry lookup is attempted.
    #[test]
    fn expand_refs_ref_without_hash_prefix_passes_through() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).expect("empty registry builds fine");
        let schema = json!({ "$ref": "SomeName" });
        let expanded = reg
            .expand_refs(&schema)
            .expect("no-hash $ref must not error");
        assert_eq!(
            expanded, schema,
            "$ref without '#' prefix must pass through unchanged"
        );
    }

    // JSON Pointer $ref nested inside properties must also pass through
    // unchanged — the slash-filter guard applies identically in the recursive
    // call, not only at the top-level entry point.
    #[test]
    fn expand_refs_nested_json_pointer_ref_passes_through() {
        let reg = SchemaRegistry::build(&BTreeMap::new()).expect("empty registry builds fine");
        let schema = json!({
            "type": "object",
            "properties": {
                "x": { "$ref": "#/definitions/X" }
            }
        });
        let expanded = reg
            .expand_refs(&schema)
            .expect("nested JSON Pointer $ref must not error");
        assert_eq!(
            expanded, schema,
            "nested JSON Pointer $ref must pass through unchanged"
        );
    }
}
