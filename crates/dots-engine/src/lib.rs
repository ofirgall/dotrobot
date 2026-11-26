//! The reconcile engine: `plan` = diff(resolution, installed) into ordered, batched steps;
//! `apply` confirms, runs them (independent providers in parallel, one sudo prompt, failures
//! isolated) and records state and journal after every action. See `specs/reconcile-engine`.
//!
//! Files are not done here: the engine calls a [`FilesPhase`] after the package phase.

mod apply;
mod files;
mod journal;
mod lock;
mod plan;
mod render;
mod state;
mod version;

pub use apply::{
    ApplyConfig, ApplyReport, Mode, NoObserver, NoPrompt, Observer, Prompter, Status, StepResult,
    StepStatus, apply, apply_observed,
};
pub use files::{FileStep, FileStepResult, FilesPhase, FilesRequest, NoFiles, Payload};
pub use journal::{Entry as JournalEntry, Journal, JournalMeta, new_run_id, rfc3339};
pub use lock::RunLock;
pub use plan::{Batch, Inputs, PkgStep, Plan, Scope, StepKind, plan, select};
pub use render::{plan_json, plan_text};
pub use state::{OwnedFile, OwnedPackage, STATE_SCHEMA, State, Todo};
pub use version::{Change, Wanted, compare, diff, normalize, parse_wanted};
