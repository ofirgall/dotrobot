//! Renderers (rich / plain / json) built from reports, terminal tier detection, the pager and
//! the progress API. This crate owns what the binary writes to stdout and stderr.

mod config;
mod emit;
mod layout;
mod pager;
mod progress;
mod render;
mod theme;
mod tier;
mod ui;

pub use config::{IconSet, ThemeChoice, Toggle, UiConfig};
pub use emit::{emit, load_config};
pub use layout::{truncate_end, truncate_middle, width};
pub use pager::{page, run_pager};
pub use progress::{Progress, ProgressSink, TaskId};
pub use render::{Blocks, Rendered, render, render_with};
pub use theme::{paint, state_symbol};
pub use tier::{Tier, detect_tier, detect_tier_with};
pub use ui::{COMPACT_BELOW, DEFAULT_PAGER, Term, Theme, Ui, detect_theme};

/// Which output form a command run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Json,
    Human(Tier),
}
