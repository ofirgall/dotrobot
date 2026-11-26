//! Schema types and strict loaders for `dots.toml`, bundles, packages and machine files.
//!
//! Parsing is strict (unknown keys, wrong types and unsupported schemas are errors with
//! `file:line:col` and "did you mean" hints) and collects every error instead of stopping at
//! the first. Entry points: [`Repo::load`] and [`MachineConfig::load`].

mod defs;
mod load;
mod machine;
mod repo;
mod schema;
mod source;
mod spans;
mod types;
pub mod version;

pub use defs::BUILTIN_PROVIDERS;
pub use load::SUPPORTED_SCHEMA;
pub use machine::MachineConfig;
pub use repo::{LoadErrors, Loaded, Repo};
pub use schema::escapes_package;
pub use spans::Spans;
pub use types::*;
pub use version::check_requires_dots;
