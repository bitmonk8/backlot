//! Validation reporting types: [`Location`], [`ValidationIssue`], [`ValidationReport`].

use std::path::{Path, PathBuf};

/// Source location for a validation issue. All fields are optional.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Location {
    /// Workflow file path (passed in by the caller).
    pub file: Option<PathBuf>,
    /// Function name within the workflow.
    pub function: Option<String>,
    /// Block name within the function.
    pub block: Option<String>,
    /// Field name within the block (e.g. `prompt`, `transitions[0].when`).
    pub field: Option<String>,
}

impl Location {
    pub(crate) fn root(file: Option<&Path>) -> Self {
        Self {
            file: file.map(Path::to_path_buf),
            ..Self::default()
        }
    }

    pub(crate) fn with_function(mut self, name: &str) -> Self {
        self.function = Some(name.to_string());
        self
    }

    pub(crate) fn with_block(mut self, name: &str) -> Self {
        self.block = Some(name.to_string());
        self
    }

    pub(crate) fn with_field(mut self, name: impl Into<String>) -> Self {
        self.field = Some(name.into());
        self
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = &self.file {
            parts.push(p.display().to_string());
        }
        if let Some(fun) = &self.function {
            parts.push(format!("function `{fun}`"));
        }
        if let Some(b) = &self.block {
            parts.push(format!("block `{b}`"));
        }
        if let Some(field) = &self.field {
            parts.push(format!("field `{field}`"));
        }
        if parts.is_empty() {
            f.write_str("<workflow>")
        } else {
            f.write_str(&parts.join(" / "))
        }
    }
}

/// A single validation finding (error or warning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// Where in the workflow the issue was found.
    pub location: Location,
    /// Human-readable description of the problem.
    pub message: String,
}

impl ValidationIssue {
    pub(crate) fn new(location: Location, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

/// Result of validating a workflow file.
#[derive(Debug, Default, Clone)]
pub struct ValidationReport {
    /// Errors prevent execution.
    pub errors: Vec<ValidationIssue>,
    /// Warnings are informational and do not prevent execution.
    pub warnings: Vec<ValidationIssue>,
}

impl ValidationReport {
    /// True iff there are no errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}
