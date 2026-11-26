//! Facts, the platform chain and the condition language (platform-conditions capability).
//!
//! - [`detect`] builds [`Facts`] from injectable [`FactSources`].
//! - [`Chain`] is the general-to-specific platform chain.
//! - [`Condition`] parses and evaluates `cfg()`-style `when` expressions.

mod chain;
mod cond;
mod facts;
mod sources;

pub use chain::Chain;
pub use cond::{
    CondError, Condition, EvalCtx, MapSettings, NoSettings, References, SettingLookup, SettingRef,
    compare_versions,
};
pub use facts::{
    FACT_NAMES, FactOverrides, Facts, Source, Value, detect, parse_os_release, suggest,
};
pub use sources::{CmdOutput, FactSources, FakeSources, RealSources};
