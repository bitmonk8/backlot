//! Model checker trait and built-in implementations.

use std::collections::HashSet;

/// Trait for resolving an agent model name to "exists" / "does not exist".
///
/// Production code should wrap flick's `ModelRegistry`. Tests typically use
/// [`AnyModel`] or [`KnownModels`] to avoid filesystem access.
pub trait ModelChecker {
    /// Returns `true` if the named model is known.
    fn knows(&self, model: &str) -> bool;
}

/// [`ModelChecker`] that accepts every model name. Useful for tests where
/// model resolution is not under test.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnyModel;

impl ModelChecker for AnyModel {
    fn knows(&self, _model: &str) -> bool {
        true
    }
}

/// [`ModelChecker`] backed by a fixed set of known model names.
#[derive(Debug, Clone, Default)]
pub struct KnownModels {
    names: HashSet<String>,
}

impl KnownModels {
    /// Build from any iterator of names.
    pub fn new<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }
}

impl ModelChecker for KnownModels {
    fn knows(&self, model: &str) -> bool {
        self.names.contains(model)
    }
}
