//! The reconcile engine: `plan` = diff(resolution, installed) into ordered, batched steps;
//! `apply` confirms, runs them (independent providers in parallel, one sudo prompt, failures
//! isolated) and records state and journal after every action. See `specs/reconcile-engine`.
//!
//! Files, settings and services are not done here: the engine calls a [`FilesPhase`], a
//! [`SettingsPhase`] and a [`ServicesPhase`], in that order, after the package phase. Package
//! hooks run last, through a [`HooksPhase`] (see the `hooks` module for their semantics).

mod apply;
mod files;
mod hooks;
mod journal;
mod lock;
mod locks;
mod phases;
mod plan;
mod render;
mod shims;
mod state;
mod version;

pub use apply::{
    ApplyConfig, ApplyReport, LockDrop, Mode, NoObserver, NoPrompt, Observer, Prompter,
    RemovalResult, Status, StepResult, StepStatus, apply, apply_observed,
};
pub use files::{FileStep, FileStepResult, FilesPhase, FilesRequest, NoFiles, Payload};
pub use hooks::{HookRequest, HookStep, HookStepResult, HooksPhase, NoHooksPhase};
pub use journal::{Entry as JournalEntry, Journal, JournalMeta, new_run_id, rfc3339};
pub use lock::RunLock;
pub use locks::{LockInputs, LockStep, LockUpdate, UpgradeTarget, source_of, version_and_rev};
pub use phases::{
    NoServicesPhase, NoSettingsPhase, PlanError, RestartResult, ServiceStep, ServiceStepResult,
    ServicesPhase, ServicesRequest, SettingStep, SettingStepResult, SettingsPhase, SettingsRequest,
};
pub use plan::{Batch, Inputs, PkgStep, Plan, Scope, StepKind, Switch, plan, select};
pub use render::{plan_json, plan_text};
pub use shims::{OwnedShim, ShimDecl, ShimState, ShimStep, ShimStepResult};
pub use shims::{declared_for as shim_declared_for, state_of as shim_state};
pub use state::{OwnedFile, OwnedPackage, STATE_SCHEMA, State, Todo};
pub use version::{Change, Wanted, compare, diff, is_outdated, normalize, parse_wanted};
