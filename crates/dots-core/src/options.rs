//! Global flags accepted by every command.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalOptions {
    pub json: bool,
    /// 0 normal, 1 `-v`, 2 `-vv`.
    pub verbose: u8,
    pub quiet: bool,
    /// `None` when `--color` was not given (so `DOTS_UI` and the environment apply).
    pub color: Option<ColorChoice>,
    pub no_input: bool,
    pub offline: bool,
    pub machine_config: Option<PathBuf>,
    /// Redirects `~` and all four dots directories.
    pub home: Option<PathBuf>,
}
