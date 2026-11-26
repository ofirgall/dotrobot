//! System settings: macOS `defaults` domains, Windows registry values and Linux `xdg` defaults.
//!
//! Declared on a package (`[[defaults]]`, `[[registry]]`, `[[xdg]]`), read before they are
//! written (one batched read per domain, key or mime type), compared with the file state
//! vocabulary, and restored to their original value when the declaration goes. Every OS call
//! goes through a [`dots_providers::Runner`].
//!
//! Originals live in `<state>/settings.json` ([`Ledger`]), a sibling of the files ledger.

mod adopt;
mod ledger;
mod model;
mod plan;
pub mod plist;
mod sys;

pub use adopt::{Target, adopt, adopt_in, locate};
pub use ledger::{Change, LEDGER_SCHEMA, Ledger, Orig, Owned};
pub use model::{Declared, Setting, Split, declared};
pub use plan::{
    Health, Op, Restored, Row, Settled, Step, apply_step, apply_step_noted, needs, plan, restarts,
    restore_run, same, satisfied, settled, status, todo_reason,
};
pub use sys::{
    Cur, KeyPath, NEEDS_SUDO_READ, Observed, Sudo, Sys, TOOL_MISSING, Tools, exec, is_tool_missing,
    parse_reg_query, reg_canon, resolve_key, restart_app,
};
