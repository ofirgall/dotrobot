//! Contract layer: report model, JSON envelope, error registry, paths and global options.

pub mod blocks;
pub mod commands;
pub mod env;
pub mod errors;
pub mod exe;
pub mod exit;
pub mod options;
pub mod paths;
pub mod report;

pub use blocks::{Block, Cell, Lead, Marker, Row, Section, State, Style, Table, Tone, TreeNode};
pub use env::{Env, MapEnv, ProcessEnv};
pub use exit::ExitCode;
pub use options::{ColorChoice, GlobalOptions};
pub use paths::Dirs;
pub use report::{Diagnostic, Provenance, Report};

/// Set to `1` to regenerate checked-in generated files (docs, schemas) from tests.
pub const REGEN_ENV: &str = "DOTS_REGEN";
