//! The seams to the settings and services phases. Like [`crate::FilesPhase`], the engine only
//! orders them (after files: settings, then services, before hooks), shows their steps and
//! records what they did; the real implementations live in `dots-settings` and `dots-services`
//! and are wired in by the CLI. [`NoSettingsPhase`] and [`NoServicesPhase`] plan nothing.

use std::path::Path;

use dots_core::Diagnostic;

use crate::files::Payload;

/// Why a phase could not plan. A plain message is wrapped as `plan-failed`; a phase that has a
/// registered code of its own (`service-name-invalid`) passes it as a [`Diagnostic`], which
/// reaches the report and the JSON `errors[].code` untouched.
#[derive(Debug, Clone)]
pub enum PlanError {
    Message(String),
    Coded(Diagnostic),
}

impl From<String> for PlanError {
    fn from(m: String) -> Self {
        PlanError::Message(m)
    }
}

impl From<&str> for PlanError {
    fn from(m: &str) -> Self {
        PlanError::Message(m.to_string())
    }
}

impl From<Diagnostic> for PlanError {
    fn from(d: Diagnostic) -> Self {
        PlanError::Coded(d)
    }
}

impl PlanError {
    /// The diagnostic for the report: `what` names the phase for a plain message.
    pub fn into_diagnostic(self, what: &str) -> Diagnostic {
        match self {
            PlanError::Coded(d) => d,
            PlanError::Message(m) => {
                Diagnostic::from_code("plan-failed", &[("package", what), ("detail", &m)])
            }
        }
    }
}

// ---------------------------------------------------------------------------- settings

/// One planned change of a system setting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingStep {
    pub package: String,
    /// Stable identity (`defaults:NSGlobalDomain:KeyRepeat`), also the `dots adopt` argument.
    pub id: String,
    /// What the user reads (`NSGlobalDomain KeyRepeat`).
    pub label: String,
    /// `defaults`, `registry` or `xdg`.
    pub kind: String,
    /// `set`, `restore` (the declaration is gone) or `delete` (the key did not exist before).
    pub action: String,
    /// `6 -> 2`.
    pub detail: String,
    /// Runs with sudo, inside the run's one prompt.
    pub root: bool,
    /// Apps to restart once at the end of the run.
    pub restart: Vec<String>,
    /// `logout`, `reboot` or `restart-app:<name>`.
    pub requires: Vec<String>,
    pub payload: Payload,
}

/// What the settings phase is asked to plan or apply.
#[derive(Debug, Clone, Copy)]
pub struct SettingsRequest<'a> {
    pub run_id: &'a str,
    /// The active packages in scope; for `apply` already reduced to those whose install did
    /// not fail.
    pub packages: &'a [String],
    pub home: &'a Path,
    pub state_dir: &'a Path,
    pub dry_run: bool,
    /// The whole repo is in scope (no package was named), so settings of packages that are
    /// gone from the repo are restored too.
    pub full: bool,
    /// Root steps go through `sudo -n`: the run primed sudo once.
    pub sudo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingStepResult {
    pub step: SettingStep,
    pub ok: bool,
    pub detail: Option<String>,
    pub dur_ms: u64,
}

/// An app restart at the end of the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartResult {
    pub app: String,
    pub ok: bool,
    pub detail: Option<String>,
}

pub trait SettingsPhase: Send + Sync {
    /// The setting changes needed for `req.packages`, in apply order. Pure: no side effects.
    fn plan(&self, _req: &SettingsRequest<'_>) -> Result<Vec<SettingStep>, PlanError> {
        Ok(Vec::new())
    }

    /// Apply `steps` in order, calling `done` after each. Must not run anything when
    /// `req.dry_run`. An `Err` aborts the phase; steps already reported stay reported.
    fn apply(
        &self,
        _req: &SettingsRequest<'_>,
        _steps: &[SettingStep],
        _done: &mut dyn FnMut(SettingStepResult),
    ) -> Result<(), String> {
        Ok(())
    }

    /// The manual actions (`logout` / `reboot` requirements raised by an earlier run) whose
    /// setting reads as declared now: `(package, reason)`, the reason built with
    /// `dots_settings::todo_reason`. The engine closes them before applying settings.
    fn settled(&self, _req: &SettingsRequest<'_>) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Restart `apps`, each exactly once. The engine collects them across the whole run (the
    /// `restart` of every changed entry and `restart-app:<name>` requirements) and calls this
    /// once, after the last setting change.
    fn restart(&self, _req: &SettingsRequest<'_>, _apps: &[String]) -> Vec<RestartResult> {
        Vec::new()
    }
}

/// No settings at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSettingsPhase;
impl SettingsPhase for NoSettingsPhase {}

// ---------------------------------------------------------------------------- services

/// One planned service action.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceStep {
    pub package: String,
    pub name: String,
    /// `brew`, `launchd`, `systemd-user` or `windows-startup`.
    pub manager: String,
    /// `enable`, `start`, `enable+start`, `restart`, ...
    pub action: String,
    /// Why (`not enabled`, `config changed`, `not running`).
    pub detail: String,
    /// The exact commands, one per line, for the plan.
    pub commands: Vec<String>,
    /// Takes down a service whose declaration is gone. Teardown runs BEFORE the files phase
    /// (see [`ServicesRequest::teardown`]).
    pub teardown: bool,
    pub payload: Payload,
}

#[derive(Debug, Clone, Copy)]
pub struct ServicesRequest<'a> {
    pub run_id: &'a str,
    pub packages: &'a [String],
    pub home: &'a Path,
    pub state_dir: &'a Path,
    pub dry_run: bool,
    /// Packages whose config files or unit change in this run: their service restarts once
    /// (when running).
    pub changed: &'a [String],
    /// This call carries only the teardown steps, and runs before the files phase: a dropped
    /// package's unit file is still linked, so `systemctl disable --now` finds its unit. The
    /// phase must not record config baselines on such a call; the later call (all the other
    /// steps) does.
    pub teardown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStepResult {
    pub step: ServiceStep,
    pub ok: bool,
    pub detail: Option<String>,
    pub dur_ms: u64,
}

pub trait ServicesPhase: Send + Sync {
    /// The service actions needed, in apply order. Pure: no side effects.
    fn plan(&self, _req: &ServicesRequest<'_>) -> Result<Vec<ServiceStep>, PlanError> {
        Ok(Vec::new())
    }

    /// Apply `steps` in order, calling `done` after each. Must not run anything when
    /// `req.dry_run`.
    fn apply(
        &self,
        _req: &ServicesRequest<'_>,
        _steps: &[ServiceStep],
        _done: &mut dyn FnMut(ServiceStepResult),
    ) -> Result<(), String> {
        Ok(())
    }
}

/// No services at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoServicesPhase;
impl ServicesPhase for NoServicesPhase {}
