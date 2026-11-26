//! Version constraints per package, with provenance.
//!
//! Version specs are opaque strings in v0.1: they are collected here (own spec, every
//! dependent's `requires`) so task 5.8 can add the grammar, intersection and `version-conflict`
//! errors without changing the shape of the resolver. Overrides (v0.2) add a
//! [`ConstraintSource::Override`] entry.

use crate::model::Step;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstraintSource {
    /// The package's own `version`.
    Own { file: String },
    /// A dependent's `requires = [{ name, version }]`.
    Requires {
        from: String,
        file: String,
        path: Vec<Step>,
    },
    /// An override replacing the package's own spec (not used before v0.2).
    Override { file: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    /// The raw spec, uninterpreted.
    pub spec: String,
    pub source: ConstraintSource,
}

/// Every constraint on one package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConstraintSet {
    pub constraints: Vec<Constraint>,
}

impl ConstraintSet {
    pub fn is_empty(&self) -> bool {
        self.constraints.is_empty()
    }
}

/// Can `provider` deliver a version satisfying `set`?
///
/// Hook for task 5.8 (constraint-aware selection). It will compare the intersected constraint
/// with what the provider offers (brew: its current version; apt: `apt-cache policy`; `git:`
/// needs a git-capable provider). Until the grammar exists nothing can be compared, so every
/// provider can satisfy everything. `Err` carries the skip reason shown to the user, such as
/// `offers 10.2.0, need 8.7`.
pub fn can_satisfy(_provider: &str, _package: &str, _set: &ConstraintSet) -> Result<(), String> {
    Ok(())
}
