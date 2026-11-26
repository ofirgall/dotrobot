//! Package resolution: bundles to a concrete, ordered, explainable package set.
//!
//! Pipeline (package-resolution spec, tasks 2.6-2.8):
//!
//! 1. machine bundles expanded through `include`, plus the transitive closure of `requires`;
//! 2. `exclude` removed (status `excluded`), packages whose `when` is false marked inert;
//! 3. a provider chosen for every active package (prefer / priority / availability / sudo /
//!    sticky), injecting the bootstrap package when the provider needs one;
//! 4. `replaces` applied, `conflicts` and binary collisions reported;
//! 5. a topological order over `requires`, `after` and bootstrap edges, with cycles reported.
//!
//! [`resolve`] is pure given its [`Inputs`]: provider availability comes from a
//! [`dots_providers::Registry`] and installed state from a caller-supplied [`InstalledMap`],
//! so tests drive it with `FakeProvider`.
//!
//! Version specs follow the grammar in `dots_model::vspec`; [`constraints`] keeps a constraint
//! set per package with provenance, intersects it (`version-conflict` names both sides) and
//! checks whether a provider can deliver the result.

pub mod constraints;
mod diag;
mod graph;
mod lint;
mod model;
mod pass;
mod spec;

pub use constraints::{
    Constraint, ConstraintSet, ConstraintSource, NoOffers, OfferSource, can_satisfy,
    offers_only_current,
};
pub use lint::{LintInputs, LintReport, lint};
pub use model::*;
pub use pass::{
    Inputs, InstalledMap, NoSettingsSource, SettingsSource, resolve, resolve_full, resolve_with,
};
pub use spec::{spec_for, spec_for_in};
