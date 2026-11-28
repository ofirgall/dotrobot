//! File management: modes (link, copy, render, inject, dir), layers, format-aware merges,
//! generated headers, ownership, backups, atomic writes and drift states.
//!
//! The engine calls three entry points:
//!
//! * [`resolve_entry`] turns a package's `[[files]]` entry into a [`ResolvedFile`];
//! * [`plan`] turns resolved files into serializable [`FileStep`]s (nothing is written);
//! * [`apply`] performs one step; [`status`] reports drift; [`plan_removals`] covers owned
//!   paths that are no longer desired; [`restore`] puts backups back.

mod adopt;
mod diff3;
mod directives;
mod fragments;
mod fsops;
mod ledger;
mod merge;
mod model;
mod ops;
mod pathvars;
mod render;
mod resolve;
mod restore;
mod scope;
mod set;
mod status;
mod structured;
pub mod terminfo;

pub use adopt::{AdoptOpts, Adopted, adopt};
pub use diff3::{Conflict, Merged, merge3};
pub use directives::lint_layers;
pub use fragments::{FRAGMENT_OWNER, FragmentSource, compose_fragments, lint_collisions};
pub use fsops::{
    LinkKind, Stage, atomic_symlink, atomic_write, atomic_write_follow, atomic_write_hooked,
    make_link, refuse_symlinks, resolve_link, sha256_hex, symlinks_refused,
};
pub use ledger::{Ledger, Record};
pub use merge::{DELETE_MARKER, Node, Strategy};
pub use model::{
    Action, BackupKind, CondEnv, Extras, FileOutcome, FileState, FileStatus, FileStep, FilesCtx,
    LayerSrc, Mode, OutcomeStatus, Resolution, ResolvedFile, SetPatch, SettingUse,
};
pub use ops::{apply, plan, plan_removals};
pub use pathvars::{expand_path, pick_platform, resolve_path_vars};
pub use render::render_layers;
pub use resolve::{detect_format, entry_dst, infer_comment, override_layer_rel, resolve_entry};
pub use restore::{RestoreReport, list_runs, restore};
pub use status::status;
