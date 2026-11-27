//! Schema types and strict loaders for `dots.toml`, bundles, packages and machine files.
//!
//! Parsing is strict (unknown keys, wrong types and unsupported schemas are errors with
//! `file:line:col` and "did you mean" hints) and collects every error instead of stopping at
//! the first. Entry points: [`Repo::load`] and [`MachineConfig::load`].

mod address;
mod defs;
mod effective;
mod explain;
mod load;
mod machine;
pub mod manual;
mod merge;
mod repo;
mod schema;
mod setting;
mod source;
mod spans;
mod types;
pub mod version;
pub mod vspec;

pub use address::{Address, Found, Sub};
pub use defs::BUILTIN_PROVIDERS;
pub use effective::{
    EffObject, EffectiveInfo, Located, Miss, OverrideFile, OverrideLocation, PlatformKeyRef, Prov,
    Scope, Source,
};
pub use explain::{Change, Origin, RenderLine, Rendered};
pub use load::SUPPORTED_SCHEMA;
pub use machine::MachineConfig;
pub use manual::{After, ManualAction, Requires};
pub use repo::{LoadErrors, Loaded, Repo};
pub use schema::escapes_package;
pub use setting::DeclProblem;
pub use spans::Spans;
pub use types::*;
pub use version::check_requires_dots;
