//! Error types for the mech crate.
//!
//! Covers the five runtime error categories from `docs/MECH_SPEC.md` §10.2
//! plus load-time variants, including the aggregated `WorkflowValidation`
//! variant populated by `validate.rs`.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// All errors produced by the mech crate.
///
/// Variants are split into two groups:
///
/// * **Runtime errors** (per spec §10.2): raised while executing a workflow.
/// * **Load-time errors**: raised while reading, parsing, or validating a
///   workflow file. This includes the aggregated `WorkflowValidation` variant
///   produced by `validate.rs`.
#[derive(Debug, Error)]
pub enum MechError {
    // ---- Runtime errors (§10.2) -------------------------------------------
    /// LLM output failed to validate against the block's declared JSON Schema.
    #[error("schema validation failure in block '{block}': {details}")]
    SchemaValidationFailure {
        /// Name of the block whose output failed validation.
        block: String,
        /// Human-readable description of the schema violation.
        details: String,
        /// The raw LLM output that failed to validate.
        raw_output: String,
    },

    /// A CEL guard expression raised an error during evaluation.
    ///
    /// Per spec §10.2 this is treated as non-fatal at runtime (the guard is
    /// considered false), but a typed variant is still produced for logging.
    #[error("guard evaluation error in block '{block}' for expression `{expression}`: {message}")]
    GuardEvaluationError {
        /// Name of the block containing the guard.
        block: String,
        /// The guard expression source text.
        expression: String,
        /// Underlying CEL error message.
        message: String,
    },

    /// A `{{ ... }}` template reference could not be resolved.
    #[error(
        "template resolution error in block '{block}' for expression `{expression}`: {message}"
    )]
    TemplateResolutionError {
        /// Name of the block containing the template.
        block: String,
        /// The template expression source text.
        expression: String,
        /// Underlying resolution error message.
        message: String,
    },

    /// The underlying flick/reel LLM call failed.
    #[error("LLM call failure in block '{block}': {message}")]
    LlmCallFailure {
        /// Name of the block whose LLM call failed.
        block: String,
        /// Provider error message.
        message: String,
    },

    /// A per-block timeout was exceeded.
    #[error("timeout in block '{block}' after {duration:?}")]
    Timeout {
        /// Name of the block that timed out.
        block: String,
        /// Configured timeout duration.
        duration: Duration,
    },

    /// A CEL expression failed to compile.
    #[error("CEL compile error in `{source_text}`: {message}")]
    CelCompilation {
        /// The source text of the expression.
        source_text: String,
        /// Underlying parser error message.
        message: String,
    },

    /// A CEL expression failed to evaluate.
    #[error("CEL evaluation error in `{source_text}`: {message}")]
    CelEvaluation {
        /// The source text of the expression.
        source_text: String,
        /// Underlying execution error message.
        message: String,
    },

    /// Failed to bind a namespace variable for CEL evaluation.
    ///
    /// This is distinct from [`CelEvaluation`] — the expression hasn't been
    /// reached yet; the failure is in converting JSON data to CEL values.
    #[error("CEL namespace bind error for '{namespace}': {message}")]
    CelNamespaceBind {
        /// The namespace name that failed to bind.
        namespace: String,
        /// Underlying conversion error message.
        message: String,
    },

    /// A CEL expression returned a value of the wrong type.
    #[error("CEL type error in `{source_text}`: expected {expected}, got {got}")]
    CelType {
        /// The source text of the expression.
        source_text: String,
        /// Expected CEL type (e.g. `bool`).
        expected: String,
        /// Actual CEL type produced.
        got: String,
    },

    /// A `{{ ... }}` template string failed to parse.
    #[error("template parse error in `{source_text}`: {message}")]
    TemplateParse {
        /// The source text of the template string.
        source_text: String,
        /// Parser error message.
        message: String,
    },

    // ---- Load-time errors -------------------------------------------------
    /// Failed to read a workflow file from disk.
    #[error("io error reading {path}: {source}")]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse a workflow YAML file.
    #[error("yaml parse error{}: {message}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    YamlParse {
        /// Path that failed to parse, if loaded from disk.
        path: Option<PathBuf>,
        /// Underlying parser error message.
        message: String,
    },

    /// A schema `$ref` referenced an unknown shared schema name.
    #[error("unresolved schema $ref: '{name}'")]
    SchemaRefUnresolved {
        /// The unresolved schema name.
        name: String,
    },

    /// A schema `$ref` form is malformed (e.g. not `$ref:#name` or `$ref:path`).
    #[error("malformed schema $ref: '{raw}'")]
    SchemaRefMalformed {
        /// The raw $ref string as it appeared in the workflow file.
        raw: String,
    },

    /// A workflow-level shared schema (or chain of `extends`-style refs) forms
    /// a cycle. Detected at registry construction time.
    #[error("circular schema $ref involving: {chain}", chain = chain.join(" -> "))]
    SchemaRefCircular {
        /// Names of the schemas participating in the cycle, in traversal order.
        chain: Vec<String>,
    },

    /// A workflow-level shared schema failed JSON Schema compilation.
    #[error("invalid JSON Schema for shared schema '{name}': {message}")]
    SchemaInvalid {
        /// Name of the offending shared schema.
        name: String,
        /// Underlying jsonschema compilation error message.
        message: String,
    },

    /// An inline JSON Schema (on a block or function) failed compilation.
    #[error("invalid inline JSON Schema: {message}")]
    InlineSchemaInvalid {
        /// Underlying jsonschema compilation error message.
        message: String,
    },

    /// A JSON value failed validation against a resolved schema.
    ///
    /// `path` is the JSON Pointer to the failing field within the instance
    /// (empty string for the root).
    #[error("schema validation failed at `{path}`: {message}")]
    SchemaValidationFailed {
        /// JSON Pointer to the failing field within the instance.
        path: String,
        /// Validator error message.
        message: String,
    },

    /// Attempted to validate against a deferred `infer` schema that has not
    /// yet been resolved to a concrete JSON Schema.
    #[error("cannot validate against deferred `infer` schema")]
    SchemaInferDeferred,

    /// Function output schema inference failed (§13 Deliverable 6).
    ///
    /// Raised when a function declares `output: infer` (or omits `output:`)
    /// but its terminal blocks produce incompatible schemas, or no terminal
    /// block can supply a concrete schema.
    #[error("output inference failed for function '{function}': {message}")]
    OutputSchemaInferenceFailed {
        /// Name of the function whose output schema could not be inferred.
        function: String,
        /// Human-readable description of the failure.
        message: String,
    },

    /// Aggregated load-time workflow validation errors produced by `validate.rs`.
    #[error("validation failed with {} error(s): {}", errors.len(), errors.join("; "))]
    WorkflowValidation {
        /// All validation error messages collected during loading.
        errors: Vec<String>,
    },

    /// An internal loader invariant was violated at runtime.
    ///
    /// This indicates a bug in the loader or a corrupted [`Workflow`] handle,
    /// not a user-visible workflow error.
    #[error("internal invariant violated: {message}")]
    InternalInvariant {
        /// Description of the violated invariant.
        message: String,
    },
}

/// Convenience `Result` alias for fallible mech operations.
pub type MechResult<T> = Result<T, MechError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_formats_correctly() {
        let e = MechError::SchemaValidationFailure {
            block: "extract".into(),
            details: "missing field 'name'".into(),
            raw_output: "{}".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("extract"));
        assert!(s.contains("missing field 'name'"));
        // raw_output is kept for programmatic access but NOT in Display.
        assert!(
            !s.contains("raw output"),
            "raw_output must not appear in Display: {s}"
        );

        let e = MechError::GuardEvaluationError {
            block: "decide".into(),
            expression: "context.x > 0".into(),
            message: "undefined variable".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("decide"));
        assert!(s.contains("context.x > 0"));
        assert!(s.contains("undefined variable"));

        let e = MechError::TemplateResolutionError {
            block: "render".into(),
            expression: "{{ context.missing }}".into(),
            message: "no such field".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("render"));
        assert!(s.contains("context.missing"));

        let e = MechError::LlmCallFailure {
            block: "ask".into(),
            message: "429 rate limited".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("ask"));
        assert!(s.contains("429"));

        let e = MechError::Timeout {
            block: "slow".into(),
            duration: Duration::from_secs(30),
        };
        let s = format!("{e}");
        assert!(s.contains("slow"));
        assert!(s.contains("30"));

        let e = MechError::Io {
            path: PathBuf::from("/tmp/wf.yaml"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        };
        let s = format!("{e}");
        assert!(s.contains("/tmp/wf.yaml"));

        let e = MechError::YamlParse {
            path: Some(PathBuf::from("/tmp/wf.yaml")),
            message: "unexpected token".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("/tmp/wf.yaml"));
        assert!(s.contains("unexpected token"));

        let e = MechError::YamlParse {
            path: None,
            message: "unexpected token".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("unexpected token"));
        assert!(
            !s.contains("in "),
            "None path must not produce 'in ' prefix: {s}"
        );

        let e = MechError::SchemaRefUnresolved {
            name: "Person".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("Person"));

        let e = MechError::WorkflowValidation {
            errors: vec!["bad guard".into(), "missing block".into()],
        };
        let s = format!("{e}");
        assert!(s.contains("bad guard"));
        assert!(s.contains("missing block"));
        assert!(s.contains('2'));
    }

    #[test]
    fn error_is_send_sync() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MechError>();
    }

    #[test]
    fn mech_result_type_alias() {
        let ok: MechResult<()> = Ok(());
        assert!(ok.is_ok());
        let err: MechResult<()> = Err(MechError::SchemaRefUnresolved { name: "X".into() });
        assert!(err.is_err());
    }
}
