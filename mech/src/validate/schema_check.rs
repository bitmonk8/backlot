//! Schema validation methods on [`Validator`].

use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::schema::{ContextVarDef, MechDocument};

use super::Validator;
use super::helpers::is_valid_identifier;
use super::report::Location;

impl Validator<'_> {
    pub(crate) fn validate_json_schema_object(
        &mut self,
        v: &JsonValue,
        loc: Location,
        require_required_nonempty: bool,
    ) {
        if let Err(e) = jsonschema::validator_for(v) {
            self.err(loc.clone(), format!("invalid JSON Schema: {e}"));
            return;
        }
        let Some(obj) = v.as_object() else {
            self.err(loc, "schema must be a JSON object");
            return;
        };
        match obj.get("type").and_then(JsonValue::as_str) {
            Some("object") => {}
            Some(other) => {
                self.err(
                    loc.clone(),
                    format!("schema root type must be `object`, got `{other}`"),
                );
                return;
            }
            None => {
                self.err(loc.clone(), "schema must declare root type `object`");
                return;
            }
        }
        if require_required_nonempty {
            match obj.get("required").and_then(JsonValue::as_array) {
                Some(a) if !a.is_empty() => {}
                _ => self.err(loc, "schema `required` must be present and non-empty"),
            }
        }
    }

    pub(crate) fn validate_schema_ref_resolves(
        &mut self,
        raw: &str,
        wf: &MechDocument,
        loc: Location,
    ) {
        // Route through the canonical parser so the malformed/unsupported
        // distinction stays consistent with registry/exec call-sites.
        match crate::schema::parse_named_ref(raw) {
            Ok(name) => {
                let exists = wf
                    .workflow
                    .as_ref()
                    .is_some_and(|d| d.schemas.contains_key(name));
                if !exists {
                    self.err(
                        loc,
                        format!(
                            "schema $ref `#{name}` does not resolve to a workflow-level schema"
                        ),
                    );
                }
            }
            Err(crate::error::MechError::SchemaRefUnsupported { .. }) => {
                self.err(
                    loc,
                    format!("external file $ref `{raw}` is not supported; only `$ref:#name` references are allowed"),
                );
            }
            Err(crate::error::MechError::SchemaRefMalformed { .. }) => {
                self.err(loc, format!("malformed schema $ref: `{raw}`"));
            }
            Err(_) => {
                unreachable!("parse_named_ref returns only SchemaRefMalformed/SchemaRefUnsupported")
            }
        }
    }

    pub(crate) fn validate_context_map(
        &mut self,
        map: &BTreeMap<String, ContextVarDef>,
        loc: &Location,
    ) {
        for (name, def) in map {
            if !is_valid_identifier(name) {
                self.err(
                    loc.clone().with_field(name.clone()),
                    format!("context variable name `{name}` is not a valid identifier"),
                );
            }
            if !super::helpers::VALID_JSON_TYPES.contains(&def.ty.as_str()) {
                self.err(
                    loc.clone().with_field(format!("{name}.type")),
                    format!(
                        "context variable `{name}` has invalid JSON Schema type `{}`",
                        def.ty
                    ),
                );
                continue;
            }
            if !crate::schema::value_matches_json_type(&def.initial, &def.ty) {
                self.err(
                    loc.clone().with_field(format!("{name}.initial")),
                    format!(
                        "initial value for `{name}` is not compatible with declared type `{}`",
                        def.ty
                    ),
                );
            }
        }
    }
}
