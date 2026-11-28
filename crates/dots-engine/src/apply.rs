//! `apply`: confirm, prime sudo once, run batches on a DAG scheduler, apply files, record.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use dots_core::Diagnostic;
use dots_providers::{
    Action, Cmd, Ctx, Installed, LineSink, Provider, ProviderError, Stdin, with_line_sink, with_tty,
};

use crate::files::{FileStep, FileStepResult, FilesRequest};
use crate::hooks::{HookStep, HookStepResult, HooksPhase, run_hook};
use crate::journal::{Entry, Journal, JournalMeta, rfc3339};
use crate::lock::RunLock;
use crate::phases::{
    RestartResult, ServiceStep, ServiceStepResult, ServicesRequest, SettingStep, SettingStepResult,
    SettingsRequest,
};
use crate::plan::{Batch, Inputs, PkgStep, Plan, SudoItem, plan, relative};
use crate::render::plan_text;
use crate::shims::{self, ShimStep, ShimStepResult};
use crate::state::{OwnedFile, OwnedPackage, State, Todo};

const SUDO: &str = "/usr/bin/sudo";
/// How often the sudo credential is refreshed while a run is going.
const SUDO_KEEPALIVE: Duration = Duration::from_secs(60);

/// Asks the user things. `can_prompt` is false for `--no-input`, `--json` and a non-TTY.
pub trait Prompter: Send + Sync {
    fn can_prompt(&self) -> bool;
    /// Show `text` (a plan or a question) and return whether the user agreed.
    fn confirm(&self, text: &str) -> bool;
}

/// Never prompts.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPrompt;

impl Prompter for NoPrompt {
    fn can_prompt(&self) -> bool {
        false
    }
    fn confirm(&self, _text: &str) -> bool {
        false
    }
}

/// Live events of a run, for progress display. Every method has a no-op default; they are
/// called from the thread driving the run, never while a lock of the engine is held.
pub trait Observer: Send + Sync {
    /// The confirmed plan, before anything runs.
    fn planned(&self, _plan: &Plan) {}
    /// The one sudo prompt is about to be shown / has been answered.
    fn sudo_prompt(&self, _begin: bool) {}
    /// A provider batch starts (its packages are about to be acted on).
    fn batch_started(&self, _batch: &Batch) {}
    /// A batch finished; `results` has one entry per package of the batch.
    fn batch_finished(&self, _batch: &Batch, _results: &[StepResult]) {}
    /// The files phase is about to start with this many steps.
    fn files_started(&self, _total: usize) {}
    /// One file step was applied.
    fn file_done(&self, _result: &FileStepResult) {}
    /// The settings phase is about to start with this many steps.
    fn settings_started(&self, _total: usize) {}
    /// One setting was changed (or failed).
    fn setting_done(&self, _result: &SettingStepResult) {}
    /// The services phase is about to start with this many steps.
    fn services_started(&self, _total: usize) {}
    /// One service action was applied (or failed).
    fn service_done(&self, _result: &ServiceStepResult) {}
    /// One package hook ran (or failed).
    fn hook_done(&self, _result: &HookStepResult) {}
    /// One provider shim was created, repaired or removed (or failed).
    fn shim_done(&self, _result: &ShimStepResult) {}
    /// A package hook is about to run (its `hook_done` follows).
    fn hook_started(&self, _hook: &HookStep) {}
    /// Where the output lines of the commands of `step` go while they run (progress display
    /// only; logs are written regardless). Asked once as the step starts.
    fn output_sink(&self, _step: StepRef<'_>) -> Option<LineSink> {
        None
    }
}

/// A running step whose command output an observer may want to follow.
#[derive(Debug, Clone, Copy)]
pub enum StepRef<'a> {
    Batch(&'a Batch),
    Hook(&'a HookStep),
}

/// Observes nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoObserver;
impl Observer for NoObserver {}

/// Whether this call may change the machine.
pub enum Mode<'a> {
    /// `--dry-run`: plan only. Nothing is executed, no lock is needed.
    DryRun,
    /// A real run, holding the run lock.
    Live(&'a RunLock),
}

/// What `apply` does with sudo steps (see [`Plan::sudo_items`]). Running as root (`sudo` below
/// false) makes sudo steps ordinary: every policy but `Only` runs everything, unprompted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SudoPolicy {
    /// Plain `dots apply`: never ask for a password, never run a sudo step, skip what depends
    /// on one and say so at the end.
    #[default]
    Skip,
    /// `--with-sudo`: everything, priming sudo once at the start.
    With,
    /// `--sudo-only`: only the sudo steps (and their install hooks), priming sudo once.
    Only,
}

impl SudoPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            SudoPolicy::Skip => "skip",
            SudoPolicy::With => "with",
            SudoPolicy::Only => "only",
        }
    }
}

pub struct ApplyConfig {
    pub run_id: String,
    /// `--yes`.
    pub yes: bool,
    pub allow_downgrade: bool,
    /// Prime sudo once and prefix root actions with `sudo -n`. False when already root.
    pub sudo: bool,
    /// Which steps run: see [`SudoPolicy`].
    pub sudo_policy: SudoPolicy,
    pub journal: JournalMeta,
    /// Set from a SIGINT handler: the run stops after the current action.
    pub stop: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Done,
    /// At least one action failed.
    Failed,
    Interrupted,
    DryRun,
    /// A confirmation was needed and could not be asked (or was needed despite `--yes`).
    ConfirmationRequired,
    Declined,
    PlanFailed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Interrupted => "interrupted",
            Status::DryRun => "dry-run",
            Status::ConfirmationRequired => "confirmation-required",
            Status::Declined => "declined",
            Status::PlanFailed => "plan-failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    Ok,
    Failed,
    SkippedDueToDep {
        dependency: String,
    },
    /// Interactive step turned into a todo.
    Deferred,
    /// The run stopped before reaching it.
    NotRun,
    /// A sudo step a plain `dots apply` leaves alone (or a `--sudo-only` one whose ordinary
    /// dependency is not installed yet). Not a failure; nothing is recorded.
    SkippedSudo {
        reason: String,
    },
    /// `--sudo-only` does not apply ordinary steps.
    NotSelected,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Failed => "failed",
            StepStatus::SkippedDueToDep { .. } => "skipped-due-to-dep",
            StepStatus::Deferred => "deferred",
            StepStatus::NotRun => "not-run",
            StepStatus::SkippedSudo { .. } => "skipped-sudo",
            StepStatus::NotSelected => "not-selected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub package: String,
    pub provider: String,
    pub action: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub status: StepStatus,
    pub log: Option<PathBuf>,
    /// The failure, with the tail of the log.
    pub error: Option<String>,
    pub dur_ms: u64,
}

#[derive(Debug)]
pub struct ApplyReport {
    pub status: Status,
    pub run_id: String,
    pub plan: Plan,
    pub steps: Vec<StepResult>,
    pub files: Vec<FileStepResult>,
    /// File steps not applied because their package's step did not succeed.
    pub files_skipped: Vec<(FileStep, String)>,
    pub settings: Vec<SettingStepResult>,
    /// Setting steps not applied because their package's install failed.
    pub settings_skipped: Vec<(SettingStep, String)>,
    /// Apps restarted at the end of the run, each once.
    pub restarts: Vec<RestartResult>,
    pub services: Vec<ServiceStepResult>,
    /// Service steps not applied because their package's install or config failed.
    pub services_skipped: Vec<(ServiceStep, String)>,
    /// Provider shims created, repaired or removed.
    pub shims: Vec<ShimStepResult>,
    /// Shim steps not applied because their package did not install.
    pub shims_skipped: Vec<(ShimStep, String)>,
    /// Package hooks that ran, in the order they ran.
    pub hooks: Vec<HookStepResult>,
    /// Hooks not run, with the reason (their package did not install, files did not apply, an
    /// earlier hook of the package failed).
    pub hooks_skipped: Vec<(HookStep, String)>,
    pub todos: Vec<Todo>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    /// Lock rows for the caller to write into `dots.lock`: every successful install, upgrade
    /// or downgrade whose row changes, and every adoption or first record.
    pub lock_updates: Vec<crate::locks::LockUpdate>,
    /// Lock rows of a provider a package moved away from: the caller deletes them.
    pub lock_drops: Vec<LockDrop>,
    /// What happened to the old copy of each provider switch.
    pub removals: Vec<RemovalResult>,
    /// The policy this run had.
    pub sudo_policy: SudoPolicy,
    /// Steps not applied because they need sudo, or depend on one that was not applied, each
    /// with the reason. Empty unless the policy skipped something.
    pub sudo_skipped: Vec<SudoItem>,
    /// `--sudo-only`: how many ordinary steps (packages, files, settings, services) were left
    /// for a plain `dots apply`.
    pub sudo_unselected: usize,
}

/// A lock row to delete after a provider switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockDrop {
    pub package: String,
    pub provider: String,
    pub platform: String,
}

/// The old copy of a provider switch: removed, kept (with the reason) or failed to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalResult {
    pub package: String,
    pub provider: String,
    pub native: String,
    pub version: Option<String>,
    pub removed: bool,
    pub detail: Option<String>,
}

impl ApplyReport {
    fn bare(status: Status, run_id: &str, plan: Plan) -> Self {
        ApplyReport {
            status,
            run_id: run_id.to_string(),
            plan,
            steps: Vec::new(),
            files: Vec::new(),
            files_skipped: Vec::new(),
            settings: Vec::new(),
            settings_skipped: Vec::new(),
            restarts: Vec::new(),
            services: Vec::new(),
            services_skipped: Vec::new(),
            shims: Vec::new(),
            shims_skipped: Vec::new(),
            hooks: Vec::new(),
            hooks_skipped: Vec::new(),
            todos: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            lock_updates: Vec::new(),
            lock_drops: Vec::new(),
            removals: Vec::new(),
            sudo_policy: SudoPolicy::default(),
            sudo_skipped: Vec::new(),
            sudo_unselected: 0,
        }
    }
}

// ------------------------------------------------------------------------------ batch worker

#[derive(Clone)]
struct ActionOutcome {
    /// Native names of the ops this action carried.
    natives: Vec<String>,
    ran: bool,
    ok: bool,
    log: Option<PathBuf>,
    error: Option<String>,
    code: Option<&'static str>,
    dur_ms: u64,
}

struct BatchOutcome {
    id: usize,
    actions: Vec<ActionOutcome>,
    installed_after: Option<BTreeMap<String, Installed>>,
    /// The `pre_install` hooks that ran before the batch.
    hooks: Vec<HookStepResult>,
}

fn log_tail(log: &Option<PathBuf>) -> String {
    let Some(p) = log else { return String::new() };
    let Ok(text) = std::fs::read_to_string(p) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(8);
    lines[from..].join("\n")
}

/// Run one action (with `sudo -n` in front when it needs root). The flag is true when the
/// command ran and simply exited non-zero (not a timeout, a coded error or a missing root).
fn exec_action(
    provider: &dyn Provider,
    ctx: &Ctx,
    action: &Action,
    use_sudo: bool,
    sudo_ok: bool,
) -> (ActionOutcome, bool) {
    let natives: Vec<String> = action.ops.iter().map(|o| o.spec.name.clone()).collect();
    let started = Instant::now();
    let mut act: Action = action.clone();
    if act.needs_root && use_sudo {
        if !sudo_ok {
            return (
                ActionOutcome {
                    natives,
                    ran: true,
                    ok: false,
                    log: None,
                    error: Some("could not get root access".into()),
                    code: Some("sudo-failed"),
                    dur_ms: 0,
                },
                false,
            );
        }
        let mut argv = vec![SUDO.to_string(), "-n".to_string()];
        argv.append(&mut act.argv);
        act.argv = argv;
    }
    let res = provider.exec(ctx, &act);
    let dur_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(o) if o.success => (
            ActionOutcome {
                natives,
                ran: true,
                ok: true,
                log: o.log,
                error: None,
                code: None,
                dur_ms,
            },
            false,
        ),
        Ok(o) => (
            ActionOutcome {
                natives,
                ran: true,
                ok: false,
                error: Some(format!("`{}` failed", action.argv.join(" "))),
                log: o.log,
                code: None,
                dur_ms,
            },
            true,
        ),
        Err(e) => {
            let (code, log) = match &e {
                ProviderError::Coded { code, log, .. } => (Some(*code), log.clone()),
                _ => (None, None),
            };
            (
                ActionOutcome {
                    natives,
                    ran: true,
                    ok: false,
                    error: Some(e.to_string()),
                    log,
                    code,
                    dur_ms,
                },
                false,
            )
        }
    }
}

fn run_batch(
    provider: &dyn Provider,
    ctx: &Ctx,
    batch: &Batch,
    stop: &AtomicBool,
    use_sudo: bool,
    sudo_ok: bool,
    pre: (&dyn HooksPhase, &[HookStep]),
) -> BatchOutcome {
    // `pre_install` hooks of the batch's packages, before anything is installed. The batch is
    // one provider call, so a failing hook fails every package of the batch.
    let mut hooks = Vec::new();
    for h in pre.1 {
        let r = run_hook(pre.0, ctx, h);
        let failed = (!r.ok).then(|| {
            format!(
                "the {} hook of `{}` failed: {}",
                h.event,
                h.package,
                r.detail.clone().unwrap_or_default()
            )
        });
        let log = r.log.clone();
        hooks.push(r);
        if let Some(msg) = failed {
            let actions = batch
                .actions
                .iter()
                .map(|a| ActionOutcome {
                    natives: a.ops.iter().map(|o| o.spec.name.clone()).collect(),
                    ran: true,
                    ok: false,
                    log: log.clone(),
                    error: Some(msg.clone()),
                    code: None,
                    dur_ms: 0,
                })
                .collect();
            return BatchOutcome {
                id: batch.id,
                actions,
                installed_after: None,
                hooks,
            };
        }
    }
    let mut actions = Vec::new();
    let mut prep_failed = false;
    for action in &batch.actions {
        let natives: Vec<String> = action.ops.iter().map(|o| o.spec.name.clone()).collect();
        if stop.load(Ordering::SeqCst) {
            actions.push(ActionOutcome {
                natives,
                ran: false,
                ok: false,
                log: None,
                error: None,
                code: None,
                dur_ms: 0,
            });
            continue;
        }
        if prep_failed {
            actions.push(ActionOutcome {
                natives,
                ran: true,
                ok: false,
                log: None,
                error: Some("a preparatory step of this batch failed".into()),
                code: None,
                dur_ms: 0,
            });
            continue;
        }
        let (out, plain_failure) = exec_action(provider, ctx, action, use_sudo, sudo_ok);
        // A batch of packages that failed as a whole may be down to one bad member (or to a
        // package manager that stopped at the first failure, leaving the rest half done):
        // find out what the failed command did install, keep that, and run the rest one at a
        // time, so only the packages that really fail are failures.
        if plain_failure
            && action.ops.len() > 1
            && matches!(
                action.kind,
                dots_providers::OpKind::Install | dots_providers::OpKind::Upgrade
            )
        {
            let each = provider.retry_each(action);
            // Only an install can be confirmed by presence: an upgrade leaves the package
            // installed whether or not it went through.
            let present: BTreeSet<String> = if action.kind == dots_providers::OpKind::Install {
                provider
                    .installed(ctx)
                    .map(|m| {
                        action
                            .ops
                            .iter()
                            .map(|o| o.spec.name.clone())
                            .filter(|n| m.contains_key(n))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                BTreeSet::new()
            };
            if each.is_some() || !present.is_empty() {
                let per_op = out.dur_ms / action.ops.len() as u64;
                for name in &present {
                    actions.push(ActionOutcome {
                        natives: vec![name.clone()],
                        ran: true,
                        ok: true,
                        log: out.log.clone(),
                        error: None,
                        code: None,
                        dur_ms: per_op,
                    });
                }
                let rest: Vec<String> = action
                    .ops
                    .iter()
                    .map(|o| o.spec.name.clone())
                    .filter(|n| !present.contains(n))
                    .collect();
                match each {
                    Some(each) => {
                        for one in each
                            .iter()
                            .filter(|a| a.ops.iter().any(|o| !present.contains(&o.spec.name)))
                        {
                            if stop.load(Ordering::SeqCst) {
                                actions.push(ActionOutcome {
                                    natives: one.ops.iter().map(|o| o.spec.name.clone()).collect(),
                                    ran: false,
                                    ok: false,
                                    log: None,
                                    error: None,
                                    code: None,
                                    dur_ms: 0,
                                });
                                continue;
                            }
                            actions.push(exec_action(provider, ctx, one, use_sudo, sudo_ok).0);
                        }
                    }
                    // Nothing to retry with: what is still missing failed with the batch.
                    None => actions.push(ActionOutcome {
                        natives: rest,
                        ..out
                    }),
                }
                continue;
            }
        }
        if !out.ok && action.ops.is_empty() {
            prep_failed = true;
        }
        actions.push(out);
    }
    let installed_after = if actions.iter().any(|a| a.ok) {
        provider.installed(ctx).ok()
    } else {
        None
    };
    BatchOutcome {
        id: batch.id,
        actions,
        installed_after,
        hooks,
    }
}

// ------------------------------------------------------------------------------ the run

/// How a batch relates to the system package manager lock (`/var/lib/dpkg/lock-frontend`).
///
/// A root package manager batch (apt: `needs_root` and `exclusive_lock`) holds that lock for
/// the whole of its run. A `script` or user-defined (`custom`) batch runs arbitrary commands,
/// and those very often call the package manager themselves (`apt-get install` inside an
/// install script), so overlapping the two made the script fail with `Could not get lock`.
/// The engine therefore never runs the two kinds at the same time. Everything else (brew,
/// cargo, uv, git) is independent of the lock and still runs in parallel with either.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SysLock {
    None,
    /// A root package manager batch.
    Holder,
    /// A script or custom provider batch.
    Arbitrary,
}

fn sys_lock(provider_id: &str, p: &dyn Provider) -> SysLock {
    let caps = p.caps();
    if provider_id == "script" || !dots_model::BUILTIN_PROVIDERS.contains(&provider_id) {
        SysLock::Arbitrary
    } else if caps.needs_root && caps.exclusive_lock {
        SysLock::Holder
    } else {
        SysLock::None
    }
}

/// May a batch of kind `k` start while `holders` root package manager batches and `arbitrary`
/// script batches are running?
fn lock_allows(k: SysLock, holders: usize, arbitrary: usize) -> bool {
    match k {
        SysLock::None => true,
        SysLock::Holder => arbitrary == 0,
        SysLock::Arbitrary => holders == 0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum B {
    Pending,
    Running,
    Done,
    /// Failed, skipped or deferred: dependents are skipped.
    Dead,
}

struct Run<'a> {
    plan: &'a Plan,
    results: BTreeMap<String, StepResult>,
    state: State,
    state_path: PathBuf,
    journal: Journal,
    state_dir: PathBuf,
    run_id: String,
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    todos: Vec<Todo>,
    lock_updates: Vec<crate::locks::LockUpdate>,
    lock_drops: Vec<LockDrop>,
    removals: Vec<RemovalResult>,
    hook_results: Vec<HookStepResult>,
    hooks_skipped: Vec<(HookStep, String)>,
    /// Packages one of whose hooks failed in this run.
    hook_failed: BTreeSet<String>,
}

impl Run<'_> {
    /// Journal a hook, record its outcome in `state.json` and turn a failure into an error.
    fn record_hook(&mut self, r: HookStepResult) {
        self.journal(Entry {
            op: format!("hook-{}", r.step.event),
            pkg: r.step.package.clone(),
            provider: None,
            from: None,
            to: Some(r.step.short()),
            exit: i32::from(!r.ok),
            dur_ms: r.dur_ms,
            log: r.log.as_ref().map(|p| relative(&self.state_dir, p)),
        });
        if r.ok {
            self.state.hooks_failed.remove(&r.step.key());
            self.state.hooks_pending.remove(&r.step.key());
        } else {
            self.state.hooks_failed.insert(r.step.key());
            self.hook_failed.insert(r.step.package.clone());
            let log = r
                .log
                .as_ref()
                .map_or("(no log)".to_string(), |p| p.display().to_string());
            let mut d = Diagnostic::from_code(
                "hook-failed",
                &[
                    ("package", &r.step.package),
                    ("event", &r.step.event),
                    ("detail", r.detail.as_deref().unwrap_or("the hook failed")),
                    ("log", &log),
                ],
            );
            let tail = log_tail(&r.log);
            if !tail.is_empty() {
                d.hint = format!("{}\n{tail}", d.hint);
            }
            self.errors.push(d);
        }
        self.save();
        self.hook_results.push(r);
    }

    fn save(&mut self) {
        if let Err(e) = self.state.save(&self.state_path) {
            self.warnings.push(Diagnostic::from_code(
                "write-failed",
                &[
                    ("path", &self.state_path.display().to_string()),
                    ("detail", &e.to_string()),
                ],
            ));
        }
    }

    fn journal(&mut self, e: Entry) {
        if let Err(err) = self.journal.append(&e) {
            self.warnings.push(Diagnostic::from_code(
                "write-failed",
                &[("path", "journal.jsonl"), ("detail", &err.to_string())],
            ));
        }
    }

    fn mark(&mut self, step: &PkgStep, status: StepStatus) {
        self.results.insert(
            step.package.clone(),
            StepResult {
                package: step.package.clone(),
                provider: step.provider.clone(),
                action: step.kind.as_str().into(),
                from: step.from.clone(),
                to: step.to.clone(),
                status,
                log: None,
                error: None,
                dur_ms: 0,
            },
        );
    }

    fn finish_batch(&mut self, batch: &Batch, mut out: BatchOutcome) -> bool {
        for h in std::mem::take(&mut out.hooks) {
            self.record_hook(h);
        }
        let mut all_ok = true;
        // The actions that carried a package's step.
        let actions_of = |native: &str| -> Vec<&ActionOutcome> {
            let mine: Vec<&ActionOutcome> = out
                .actions
                .iter()
                .filter(|a| a.natives.iter().any(|n| n == native) || a.natives.is_empty())
                .collect();
            if mine.is_empty() {
                out.actions.iter().collect()
            } else {
                mine
            }
        };
        // Packages that failed with one and the same command error (one batched command that
        // failed for several of them): the error is reported once, naming all of them.
        let mut failing_together: BTreeMap<(Option<String>, Option<PathBuf>), Vec<String>> =
            BTreeMap::new();
        for pkg in &batch.packages {
            let Some(step) = self.plan.step(pkg) else {
                continue;
            };
            if let Some(f) = actions_of(&step.native).iter().find(|a| a.ran && !a.ok) {
                failing_together
                    .entry((f.error.clone(), f.log.clone()))
                    .or_default()
                    .push(pkg.clone());
            }
        }
        for pkg in &batch.packages {
            let Some(step) = self.plan.step(pkg) else {
                continue;
            };
            let mine = actions_of(&step.native);
            let dur_ms: u64 = mine.iter().map(|a| a.dur_ms).sum();
            let log = mine.iter().rev().find_map(|a| a.log.clone());
            let failed = mine.iter().find(|a| a.ran && !a.ok);
            let not_run = mine.iter().any(|a| !a.ran);
            let (status, error) = match (failed, not_run) {
                (Some(f), _) => {
                    let mut msg = f.error.clone().unwrap_or_default();
                    let tail = log_tail(&f.log.clone().or(log.clone()));
                    if !tail.is_empty() {
                        msg.push('\n');
                        msg.push_str(&tail);
                    }
                    let detail = f.error.clone().unwrap_or_default();
                    let logs = log
                        .as_ref()
                        .map_or("(no log)".to_string(), |p| p.display().to_string());
                    let code = match f.code {
                        Some(c) if dots_core::errors::lookup(c).is_some() => c,
                        _ => "action-failed",
                    };
                    let group = &failing_together[&(f.error.clone(), f.log.clone())];
                    // Only the first package of a group reports; the error names them all.
                    if group.first() == Some(pkg) {
                        let diag = |packages: String| {
                            Diagnostic::from_code(
                                code,
                                &[
                                    ("package", &packages),
                                    ("action", step.kind.as_str()),
                                    ("provider", &step.provider),
                                    ("detail", &detail),
                                    ("log", &logs),
                                    ("timeout", "its limit"),
                                ],
                            )
                        };
                        let mut d = diag(group.join("`, `"));
                        // `dots apply a b`, not `dots apply a`, `b``.
                        d.hint = diag(group.join(" ")).hint;
                        let tail = log_tail(&log);
                        if !tail.is_empty() {
                            d.hint = format!("{}\n{tail}", d.hint);
                        }
                        self.errors.push(d);
                    }
                    (StepStatus::Failed, Some(msg))
                }
                (None, true) => (StepStatus::NotRun, None),
                (None, false) => (StepStatus::Ok, None),
            };
            let version = out
                .installed_after
                .as_ref()
                .and_then(|m| m.get(&step.native))
                .map(|i| i.version.clone())
                .or_else(|| step.to.clone());
            self.results.insert(
                pkg.clone(),
                StepResult {
                    package: pkg.clone(),
                    provider: step.provider.clone(),
                    action: step.kind.as_str().into(),
                    from: step.from.clone(),
                    to: version.clone(),
                    status: status.clone(),
                    log: log.clone(),
                    error,
                    dur_ms,
                },
            );
            match status {
                StepStatus::Ok => {
                    if let Some(ls) = self
                        .plan
                        .lock
                        .iter()
                        .find(|l| l.package == *pkg && l.rewrite)
                    {
                        let spec = batch
                            .actions
                            .iter()
                            .flat_map(|a| &a.ops)
                            .find(|o| o.spec.name == step.native)
                            .map(|o| o.spec.clone());
                        let inst = out
                            .installed_after
                            .as_ref()
                            .and_then(|m| m.get(&step.native))
                            .cloned()
                            .or_else(|| {
                                step.to.clone().map(|v| Installed {
                                    version: v,
                                    pinned: false,
                                    source: None,
                                })
                            });
                        if let Some(inst) = inst {
                            let spec =
                                spec.unwrap_or_else(|| dots_providers::Spec::new(&step.native));
                            let (version, rev) =
                                crate::locks::version_and_rev(&step.provider, &spec, &inst);
                            self.lock_updates.push(crate::locks::LockUpdate {
                                package: pkg.clone(),
                                provider: step.provider.clone(),
                                platform: ls.platform.clone(),
                                version,
                                rev,
                                git_ref: ls.git_ref.clone(),
                                source: crate::locks::source_of(&spec),
                                adopt: false,
                            });
                        }
                    }
                    self.state.packages.insert(
                        pkg.clone(),
                        OwnedPackage {
                            provider: step.provider.clone(),
                            name: step.native.clone(),
                            version: version.clone(),
                            run: self.run_id.clone(),
                            ts: rfc3339(SystemTime::now()),
                        },
                    );
                    self.save();
                    self.journal(Entry {
                        op: step.kind.as_str().into(),
                        pkg: pkg.clone(),
                        provider: Some(step.provider.clone()),
                        from: step.from.clone(),
                        to: version,
                        exit: 0,
                        dur_ms,
                        log: log.as_ref().map(|p| relative(&self.state_dir, p)),
                    });
                }
                StepStatus::Failed => {
                    all_ok = false;
                    self.journal(Entry {
                        op: step.kind.as_str().into(),
                        pkg: pkg.clone(),
                        provider: Some(step.provider.clone()),
                        from: step.from.clone(),
                        to: step.to.clone(),
                        exit: 1,
                        dur_ms,
                        log: log.as_ref().map(|p| relative(&self.state_dir, p)),
                    });
                }
                _ => all_ok = false,
            }
        }
        all_ok
    }
}

fn confirmation_error(reasons: &[String]) -> Diagnostic {
    let mut d = Diagnostic::from_code("confirmation-required", &[]);
    if !reasons.is_empty() {
        d.message = format!("confirmation required: {}", reasons.join("; "));
        d.hint = "Run interactively to answer, or pass `--yes` (and `--allow-downgrade` for downgrades).".into();
    }
    d
}

/// Plan, confirm and run. `Mode::DryRun` stops after the plan.
pub fn apply(
    inp: &Inputs<'_>,
    cfg: &ApplyConfig,
    prompter: &dyn Prompter,
    mode: Mode<'_>,
) -> ApplyReport {
    apply_observed(inp, cfg, prompter, mode, &NoObserver, None)
}

/// [`apply`] reporting live events to `obs`. With `env`, every successfully installed package's
/// `[env]` is folded into an overlay that later batches run with (a fresh tool is on `PATH` for
/// the packages that depend on it, in the same run).
pub fn apply_observed(
    inp: &Inputs<'_>,
    cfg: &ApplyConfig,
    prompter: &dyn Prompter,
    mode: Mode<'_>,
    obs: &dyn Observer,
    env: Option<dots_env::EnvCtx>,
) -> ApplyReport {
    let plan_v = match plan(inp, &cfg.run_id) {
        Ok(p) => p,
        Err(errors) => {
            let mut r = ApplyReport::bare(Status::PlanFailed, &cfg.run_id, Plan::default());
            r.errors = errors;
            return r;
        }
    };
    if matches!(mode, Mode::DryRun) {
        return ApplyReport::bare(Status::DryRun, &cfg.run_id, plan_v);
    }
    if plan_v.is_empty() {
        // Nothing to run; adopted and first-recorded versions still go into the lock.
        let updates = crate::locks::recorded_without_step(&plan_v.lock);
        // Nothing to apply, but a logout/reboot todo may have been done by the user since.
        if !inp.scope.packages_only && !inp.scope.files_only {
            let state_path = inp.dirs.state_json();
            let (mut state, _) = State::load(&state_path);
            let req = SettingsRequest {
                run_id: &cfg.run_id,
                packages: &plan_v.packages,
                home: &inp.dirs.home,
                state_dir: &inp.dirs.state,
                dry_run: false,
                full: inp.scope.targets.is_empty(),
                sudo: false,
            };
            if close_settled_todos(inp, &req, &mut state) {
                let _ = state.save(&state_path);
            }
        }
        let mut report = ApplyReport::bare(Status::Done, &cfg.run_id, plan_v);
        if !updates.is_empty() {
            let journal = Journal::new(&inp.dirs.journal(), &cfg.run_id, cfg.journal.clone());
            let state_path = inp.dirs.state_json();
            let (mut state, _) = State::load(&state_path);
            let mut marked = false;
            for u in updates.iter().filter(|u| u.adopt) {
                let _ = journal.append(&adopt_entry(u));
                marked |= mark_adopted_hook(inp, &u.package, &mut state);
            }
            if marked {
                let _ = state.save(&state_path);
            }
            report.lock_updates = updates;
        }
        return report;
    }

    // Confirmation policy.
    let always = plan_v.always_confirm(cfg.allow_downgrade);
    let ordinary = plan_v.touches_more_than_owned_files() && !cfg.yes;
    if ordinary || !always.is_empty() {
        if prompter.can_prompt() {
            let mut text = plan_text(&plan_v);
            if cfg.sudo && cfg.sudo_policy == SudoPolicy::Skip && plan_v.sudo_count() > 0 {
                text.push_str(&format!(
                    "\n{} need sudo and will be skipped (--with-sudo runs them, --sudo-only runs only them)",
                    plural_steps(plan_v.sudo_count())
                ));
            }
            if !prompter.confirm(&text) {
                let mut r = ApplyReport::bare(Status::Declined, &cfg.run_id, plan_v);
                r.errors.push(Diagnostic::from_code("declined", &[]));
                return r;
            }
        } else {
            let mut r = ApplyReport::bare(Status::ConfirmationRequired, &cfg.run_id, plan_v);
            r.errors.push(confirmation_error(&always));
            return r;
        }
    }

    execute(inp, cfg, prompter, plan_v, obs, env)
}

fn plural_steps(n: usize) -> String {
    if n == 1 {
        "1 step".to_string()
    } else {
        format!("{n} steps")
    }
}

fn execute(
    inp: &Inputs<'_>,
    cfg: &ApplyConfig,
    prompter: &dyn Prompter,
    plan_v: Plan,
    obs: &dyn Observer,
    env: Option<dots_env::EnvCtx>,
) -> ApplyReport {
    let state_path = inp.dirs.state_json();
    let (state, state_warn) = State::load(&state_path);
    let mut run = Run {
        plan: &plan_v,
        results: BTreeMap::new(),
        state,
        state_path,
        journal: Journal::new(&inp.dirs.journal(), &cfg.run_id, cfg.journal.clone()),
        state_dir: inp.dirs.state.clone(),
        run_id: cfg.run_id.clone(),
        errors: Vec::new(),
        warnings: Vec::new(),
        todos: Vec::new(),
        lock_updates: Vec::new(),
        lock_drops: Vec::new(),
        removals: Vec::new(),
        hook_results: Vec::new(),
        hooks_skipped: Vec::new(),
        hook_failed: BTreeSet::new(),
    };
    let mut marked = false;
    for u in crate::locks::recorded_without_step(&plan_v.lock) {
        if u.adopt {
            run.journal(adopt_entry(&u));
            marked |= mark_adopted_hook(inp, &u.package, &mut run.state);
        }
        run.lock_updates.push(u);
    }
    if marked {
        run.save();
    }
    if let Some(w) = state_warn {
        run.warnings.push(Diagnostic::from_code(
            "write-failed",
            &[("path", "state.json"), ("detail", &w)],
        ));
    }
    let _ = std::fs::create_dir_all(&inp.ctx.log_dir);
    // The engine never asks a provider to run anything in dry-run; this ctx is the live one.
    let ctx = Ctx {
        dry_run: false,
        ..inp.ctx.clone()
    };
    obs.planned(&plan_v);
    let stop: &AtomicBool = &cfg.stop;
    let mut overlay = env.map(dots_env::EnvOverlay::new);
    // A dependency installed by an earlier run has no step now, but its `[env]` (a tool on
    // `PATH`, `ZIG=...`) still has to reach what builds against it: seed the overlay with every
    // active package that is already satisfied. Packages that install in this run join as they
    // finish. Resolution covers the whole profile, so a targeted apply sees its deps too.
    if let Some(o) = overlay.as_mut() {
        for p in inp.resolution.active() {
            let pending = plan_v.steps.iter().any(|s| s.package == p.name)
                || plan_v.unplanned.iter().any(|(n, _)| *n == p.name);
            if pending {
                continue;
            }
            if let Some(pkg) = inp.repo.packages.get(&p.name) {
                o.add_package(&pkg.env);
            }
        }
    }
    let can_prompt = prompter.can_prompt() && !cfg.yes;

    // One sudo prompt for the whole run.
    // Sudo steps the policy leaves alone (the batches are marked below) never prompt.
    let skip_sudo = cfg.sudo && cfg.sudo_policy == SudoPolicy::Skip;
    let only = cfg.sudo_policy == SudoPolicy::Only;
    let needs_sudo = cfg.sudo && !skip_sudo && (plan_v.needs_root() || plan_v.sudo_count() > 0);
    let sudo_ok = if needs_sudo {
        let cmd = Cmd::new(SUDO).arg("-v").stdin(Stdin::Inherit).timeout(None);
        obs.sudo_prompt(true);
        let ok = ctx.runner.run(&cmd).is_ok_and(|o| o.success());
        obs.sudo_prompt(false);
        ok
    } else {
        true
    };

    // Repair sub-phase: links whose destination is a dangling symlink are replaced BEFORE any
    // package step. A broken link makes tools that stat the path fail (`cargo install --git`
    // stats `~/.gitconfig`), and the repo's link is exactly what fixes it. Everything else
    // stays in the files phase after the installs. A package with a `pre_files` hook keeps
    // its steps for that phase, so the hook still runs first.
    let mut files_done: Vec<FileStepResult> = Vec::new();
    let mut early_done: BTreeSet<(String, String, String)> = BTreeSet::new();
    if !only && !stop.load(Ordering::SeqCst) {
        let early: Vec<FileStep> = plan_v
            .files
            .iter()
            .filter(|f| {
                f.early
                    && !plan_v
                        .hooks
                        .iter()
                        .any(|h| h.event == "pre_files" && h.package == f.package)
            })
            .cloned()
            .collect();
        if !early.is_empty() {
            for f in &early {
                early_done.insert((f.package.clone(), f.dst.clone(), f.kind.clone()));
            }
            apply_file_steps(
                inp,
                cfg,
                obs,
                &mut run,
                &plan_v.packages,
                &early,
                &mut files_done,
            );
        }
    }
    let (ka_stop_tx, ka_stop_rx) = mpsc::channel::<()>();

    let n = plan_v.batches.len();
    let mut status = vec![B::Pending; n];
    // Batches left alone by the sudo policy before anything runs, and the batches the end
    // warning lists (those plus everything that transitively depends on them).
    let mut unselected: BTreeSet<usize> = BTreeSet::new();
    let mut listed: BTreeSet<usize> = BTreeSet::new();
    for b in &plan_v.batches {
        if skip_sudo && b.sudo {
            for p in &b.packages {
                if let Some(step) = plan_v.step(p) {
                    let reason = step.sudo.clone().unwrap_or_else(|| "needs_root".into());
                    run.mark(step, StepStatus::SkippedSudo { reason });
                }
            }
            status[b.id] = B::Dead;
            listed.insert(b.id);
        } else if only && !b.sudo {
            for p in &b.packages {
                if let Some(step) = plan_v.step(p) {
                    run.mark(step, StepStatus::NotSelected);
                }
            }
            status[b.id] = B::Dead;
            unselected.insert(b.id);
        }
    }

    std::thread::scope(|scope| {
        if needs_sudo && sudo_ok {
            let ctx = ctx.clone();
            scope.spawn(move || {
                // Refresh the credential until the run ends (the sender is dropped).
                while let Err(mpsc::RecvTimeoutError::Timeout) =
                    ka_stop_rx.recv_timeout(SUDO_KEEPALIVE)
                {
                    let _ = ctx.runner.run(&Cmd::new(SUDO).arg("-n").arg("-v"));
                }
            });
        }
        let (tx, rx) = mpsc::channel::<BatchOutcome>();
        let mut running = 0usize;
        let mut running_providers: BTreeSet<String> = BTreeSet::new();
        // Serialization keys (`[providers.script] lock`) of the batches running now.
        let mut running_locks: BTreeSet<String> = BTreeSet::new();
        // Running batches by their relation to the system package manager lock.
        let (mut running_holders, mut running_arbitrary) = (0usize, 0usize);

        loop {
            // Dependents of a dead batch are skipped (to a fixpoint).
            loop {
                let mut changed = false;
                for b in &plan_v.batches {
                    if status[b.id] != B::Pending {
                        continue;
                    }
                    let dead = b.deps.iter().find(|d| status[**d] == B::Dead).copied();
                    if let Some(d) = dead
                        && unselected.contains(&d)
                        && b.sudo
                    {
                        // `--sudo-only`: the ordinary package it needs is not installed yet.
                        for p in &b.packages {
                            if let Some(step) = plan_v.step(p) {
                                run.mark(
                                    step,
                                    StepStatus::SkippedSudo {
                                        reason: "run `dots apply` first".into(),
                                    },
                                );
                            }
                        }
                        status[b.id] = B::Dead;
                        listed.insert(b.id);
                        changed = true;
                    } else if let Some(d) = dead {
                        let dep_pkg = plan_v.batches[d]
                            .packages
                            .iter()
                            .find(|p| {
                                run.results
                                    .get(*p)
                                    .is_some_and(|r| r.status != StepStatus::Ok)
                            })
                            .cloned()
                            .unwrap_or_else(|| plan_v.batches[d].packages[0].clone());
                        for p in &b.packages {
                            if let Some(step) = plan_v.step(p) {
                                run.mark(
                                    step,
                                    StepStatus::SkippedDueToDep {
                                        dependency: dep_pkg.clone(),
                                    },
                                );
                                if !listed.contains(&d) {
                                    run.warnings.push(Diagnostic::from_code(
                                        "skipped-due-to-dep",
                                        &[("package", p), ("dependency", &dep_pkg)],
                                    ));
                                }
                            }
                        }
                        if listed.contains(&d) {
                            listed.insert(b.id);
                        }
                        status[b.id] = B::Dead;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }

            if !stop.load(Ordering::SeqCst) {
                let startable = |b: &Batch, status: &[B]| {
                    status[b.id] == B::Pending && b.deps.iter().all(|d| status[*d] == B::Done)
                };
                let plain_startable = plan_v
                    .batches
                    .iter()
                    .any(|b| !b.interactive && startable(b, &status));
                for b in &plan_v.batches {
                    if !startable(b, &status) {
                        continue;
                    }
                    let provider = inp.registry.get_for(&b.provider, &b.packages[0]);
                    let Some(provider) = provider else { continue };
                    let kind = sys_lock(&b.provider, provider);
                    if !lock_allows(kind, running_holders, running_arbitrary) {
                        continue;
                    }
                    if b.interactive {
                        if running > 0 || plain_startable {
                            continue;
                        }
                        // Under --yes / --no-input an interactive step becomes a todo.
                        let go = can_prompt
                            && prompter.confirm(&format!(
                                "Run interactive step for `{}` now?",
                                b.packages[0]
                            ));
                        if !go {
                            for p in &b.packages {
                                if let Some(step) = plan_v.step(p) {
                                    run.mark(step, StepStatus::Deferred);
                                    let t = Todo {
                                        package: p.clone(),
                                        reason: "interactive".into(),
                                        run: cfg.run_id.clone(),
                                    };
                                    run.state.todos.retain(|x| x.package != *p);
                                    run.state.todos.push(t.clone());
                                    run.todos.push(t);
                                }
                            }
                            run.save();
                            status[b.id] = B::Dead;
                            continue;
                        }
                    } else if provider.caps().exclusive_lock
                        && running_providers.contains(&b.provider)
                    {
                        continue;
                    }
                    let lock_key = provider.lock_key();
                    if lock_key.as_ref().is_some_and(|k| running_locks.contains(k)) {
                        continue;
                    }
                    status[b.id] = B::Running;
                    running += 1;
                    match kind {
                        SysLock::Holder => running_holders += 1,
                        SysLock::Arbitrary => running_arbitrary += 1,
                        SysLock::None => {}
                    }
                    obs.batch_started(b);
                    if provider.caps().exclusive_lock {
                        running_providers.insert(b.provider.clone());
                    }
                    if let Some(k) = lock_key {
                        running_locks.insert(k);
                    }
                    let tx = tx.clone();
                    // Only what packages finished so far contributed: dependencies are done
                    // before a batch starts.
                    let mut bctx = ctx.clone();
                    if let Some(o) = &overlay {
                        let base = bctx
                            .env
                            .get("PATH")
                            .map(std::ffi::OsString::from)
                            .or_else(|| std::env::var_os("PATH"));
                        bctx.env.extend(o.runner_env(base.as_deref()));
                    }
                    let use_sudo = cfg.sudo;
                    let pre_hooks: Vec<HookStep> = plan_v
                        .hooks
                        .iter()
                        .filter(|h| h.event == "pre_install" && b.packages.contains(&h.package))
                        .cloned()
                        .collect();
                    let hooks_phase = inp.hooks;
                    let sink = obs.output_sink(StepRef::Batch(b));
                    // A sudo step run with `--with-sudo` keeps the terminal: its tool may call
                    // sudo itself (a cask's .pkg installer), and the ticket primed above is
                    // keyed by the terminal.
                    let keep_tty = b.sudo && use_sudo;
                    scope.spawn(move || {
                        let out = with_line_sink(sink, || {
                            with_tty(keep_tty, || {
                                run_batch(
                                    provider,
                                    &bctx,
                                    b,
                                    stop,
                                    use_sudo,
                                    sudo_ok,
                                    (hooks_phase, &pre_hooks),
                                )
                            })
                        });
                        let _ = tx.send(out);
                    });
                    if b.interactive {
                        break;
                    }
                }
            }

            if running == 0 {
                // Nothing running; if nothing could start (finished, blocked or stopped) we are done.
                let any_startable = !stop.load(Ordering::SeqCst)
                    && plan_v.batches.iter().any(|b| {
                        status[b.id] == B::Pending
                            && b.deps.iter().all(|d| status[*d] == B::Done)
                            && inp.registry.get_for(&b.provider, &b.packages[0]).is_some()
                    });
                if !any_startable {
                    break;
                }
                continue;
            }
            let Ok(out) = rx.recv() else { break };
            running -= 1;
            let b = &plan_v.batches[out.id];
            if let Some(p) = inp.registry.get_for(&b.provider, &b.packages[0]) {
                if p.caps().exclusive_lock {
                    running_providers.remove(&b.provider);
                }
                if let Some(k) = p.lock_key() {
                    running_locks.remove(&k);
                }
                match sys_lock(&b.provider, p) {
                    SysLock::Holder => running_holders -= 1,
                    SysLock::Arbitrary => running_arbitrary -= 1,
                    SysLock::None => {}
                }
            }
            let hooks_before = run.hook_results.len();
            let ok = run.finish_batch(b, out);
            for r in &run.hook_results[hooks_before..] {
                obs.hook_done(r);
            }
            let done: Vec<StepResult> = b
                .packages
                .iter()
                .filter_map(|p| run.results.get(p).cloned())
                .collect();
            if let Some(o) = overlay.as_mut() {
                for r in done.iter().filter(|r| r.status == StepStatus::Ok) {
                    if let Some(pkg) = inp.repo.packages.get(&r.package) {
                        o.add_package(&pkg.env);
                    }
                }
            }
            obs.batch_finished(b, &done);
            status[b.id] = if ok { B::Done } else { B::Dead };
        }
        drop(ka_stop_tx);
    });

    // Provider switches: the new install is in, so the old copy can go.
    for s in &plan_v.steps {
        let Some(sw) = &s.switch else { continue };
        if run
            .results
            .get(&s.package)
            .is_none_or(|r| r.status != StepStatus::Ok)
        {
            continue;
        }
        apply_switch(inp, &ctx, cfg, sudo_ok, &mut run, &s.package, sw);
    }

    // Steps never reached.
    for s in &plan_v.steps {
        if !run.results.contains_key(&s.package) {
            run.mark(s, StepStatus::NotRun);
        }
    }

    // Files phase: only for packages whose install went through.
    let interrupted = stop.load(Ordering::SeqCst);
    // Packages whose install did not go through: their files, settings and services are left
    // alone.
    let blocked: BTreeMap<&str, &str> = plan_v
        .steps
        .iter()
        .filter_map(|s| {
            let r = run.results.get(&s.package)?;
            (r.status != StepStatus::Ok).then_some((s.package.as_str(), s.package.as_str()))
        })
        .collect();
    let eligible: Vec<String> = plan_v
        .packages
        .iter()
        .filter(|p| !blocked.contains_key(p.as_str()))
        .cloned()
        .collect();

    // Service teardown (the services of dropped packages) runs BEFORE the files phase. A
    // dropped package's unit file is removed by the files phase; disabling afterwards would
    // make `systemctl disable --now` fail on a missing unit. Running the teardown first, while
    // the unit is still linked, is the whole fix: the later services call (below) handles
    // everything else, after config and settings are in place.
    let mut services_done: Vec<ServiceStepResult> = Vec::new();
    if !interrupted && !only {
        let teardown: Vec<ServiceStep> = plan_v
            .services
            .iter()
            .filter(|s| s.teardown && !blocked.contains_key(s.package.as_str()))
            .cloned()
            .collect();
        if !teardown.is_empty() {
            let req = ServicesRequest {
                run_id: &cfg.run_id,
                packages: &eligible,
                home: &inp.dirs.home,
                state_dir: &inp.dirs.state,
                dry_run: false,
                changed: &[],
                teardown: true,
            };
            obs.services_started(teardown.len());
            apply_services(inp, obs, &mut run, &req, &teardown, &mut services_done);
        }
    }

    // The hooks phase runs commands with what this run installed on PATH.
    let hook_ctx = {
        let mut c = ctx.clone();
        if let Some(o) = &overlay {
            let base = c
                .env
                .get("PATH")
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os("PATH"));
            c.env.extend(o.runner_env(base.as_deref()));
        }
        c
    };
    let run_hooks = |run: &mut Run<'_>,
                     events: &[&str],
                     skip: &dyn Fn(&HookStep, &Run<'_>) -> Option<String>| {
        for h in plan_v.hooks.iter().filter(|h| {
            events.contains(&h.event.as_str())
                // `--sudo-only` runs the install hooks of the sudo packages and nothing else.
                && (!only
                    || (crate::hooks::is_install_event(&h.event)
                        && plan_v.step(&h.package).is_some_and(|s| s.sudo.is_some())))
        }) {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if let Some(why) = skip(h, run) {
                // A skipped setup hook is still owed: the next run offers it again.
                if h.event == "post_install" {
                    run.state.hooks_pending.insert(h.key());
                    run.save();
                }
                run.hooks_skipped.push((h.clone(), why));
                continue;
            }
            let before = run.hook_results.len();
            obs.hook_started(h);
            let sink = obs.output_sink(StepRef::Hook(h));
            let r = with_line_sink(sink, || run_hook(inp.hooks, &hook_ctx, h));
            run.record_hook(r);
            for r in &run.hook_results[before..] {
                obs.hook_done(r);
            }
        }
    };
    // Shims, right after the installs: the shim points at what the install just put on disk.
    let mut shims_done: Vec<ShimStepResult> = Vec::new();
    let mut shims_skipped: Vec<(ShimStep, String)> = Vec::new();
    if !interrupted && !only {
        let path_var = hook_ctx
            .env
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        for st in &plan_v.shims {
            if let Some(dep) = blocked.get(st.package.as_str()) {
                shims_skipped.push((st.clone(), (*dep).to_string()));
                continue;
            }
            let started = Instant::now();
            let outcome = apply_shim(inp, &mut run, st, &path_var);
            let r = ShimStepResult {
                step: st.clone(),
                ok: outcome.is_ok(),
                detail: outcome.err(),
                dur_ms: started.elapsed().as_millis() as u64,
            };
            run.journal(Entry {
                op: format!("shim-{}", st.action),
                pkg: st.package.clone(),
                provider: None,
                from: None,
                to: Some(st.dst.clone()),
                exit: i32::from(!r.ok),
                dur_ms: r.dur_ms,
                log: None,
            });
            if !r.ok {
                run.errors.push(Diagnostic::from_code(
                    "write-failed",
                    &[
                        ("path", &st.dst),
                        ("detail", r.detail.as_deref().unwrap_or("the shim failed")),
                    ],
                ));
            }
            obs.shim_done(&r);
            shims_done.push(r);
        }
    }
    let pre_skip = |h: &HookStep, run: &Run<'_>| -> Option<String> {
        if blocked.contains_key(h.package.as_str()) {
            Some("its package did not install".into())
        } else if run.hook_failed.contains(&h.package) {
            Some("an earlier hook of the package failed".into())
        } else {
            None
        }
    };
    if !interrupted && !only {
        run_hooks(&mut run, &["pre_files"], &pre_skip);
    }

    let mut files_skipped: Vec<(FileStep, String)> = Vec::new();
    if !plan_v.files.is_empty() && !interrupted && !only {
        let mut steps: Vec<FileStep> = Vec::new();
        for f in &plan_v.files {
            // Already applied (or failed) in the repair sub-phase.
            if early_done.contains(&(f.package.clone(), f.dst.clone(), f.kind.clone())) {
                continue;
            }
            match blocked.get(f.package.as_str()) {
                Some(dep) => files_skipped.push((f.clone(), (*dep).to_string())),
                // A failed `pre_files` hook leaves the package's files alone.
                None if run.hook_results.iter().any(|r| {
                    !r.ok && r.step.package == f.package && r.step.event == "pre_files"
                }) =>
                {
                    files_skipped.push((f.clone(), f.package.clone()))
                }
                None => steps.push(f.clone()),
            }
        }
        apply_file_steps(inp, cfg, obs, &mut run, &eligible, &steps, &mut files_done);
    }

    // Settings, after files and before services: only for packages whose install went through.
    // The run's one prompt primed sudo when anything needs root; when it failed `sudo` is
    // false, so the phase fails root entries instead of running them as the user.
    let settings_sudo = cfg.sudo && sudo_ok && !skip_sudo;
    if !interrupted && !inp.scope.packages_only && !inp.scope.files_only {
        let req = SettingsRequest {
            run_id: &cfg.run_id,
            packages: &eligible,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: false,
            full: inp.scope.targets.is_empty(),
            sudo: settings_sudo,
        };
        if close_settled_todos(inp, &req, &mut run.state) {
            run.save();
        }
    }
    let mut settings_done: Vec<SettingStepResult> = Vec::new();
    let mut settings_skipped: Vec<(SettingStep, String)> = Vec::new();
    let mut sudo_skipped_settings: Vec<SudoItem> = Vec::new();
    let mut restarts_done: Vec<RestartResult> = Vec::new();
    if !plan_v.settings.is_empty() && !interrupted {
        let mut steps: Vec<SettingStep> = Vec::new();
        let item = |s: &SettingStep, reason: String| SudoItem {
            name: s.label.clone(),
            what: "setting",
            kind: s.kind.clone(),
            marker: if s.action == "delete" { "-" } else { "~" },
            reason,
        };
        for s in &plan_v.settings {
            match blocked.get(s.package.as_str()) {
                Some(dep) => {
                    let sudo_blocked = plan_v
                        .step(dep)
                        .and_then(|st| run.results.get(&st.package))
                        .is_some_and(|r| {
                            matches!(
                                r.status,
                                StepStatus::SkippedSudo { .. } | StepStatus::NotSelected
                            ) || listed.contains(&plan_v.step(dep).map_or(usize::MAX, |x| x.batch))
                        });
                    if sudo_blocked && (skip_sudo || (only && s.root)) {
                        let reason = if only {
                            "run `dots apply` first".to_string()
                        } else if s.root {
                            "root".to_string()
                        } else {
                            format!("depends on {dep}")
                        };
                        sudo_skipped_settings.push(item(s, reason));
                    } else if !only || s.root {
                        settings_skipped.push((s.clone(), (*dep).to_string()));
                    }
                }
                None if skip_sudo && s.root => sudo_skipped_settings.push(item(s, "root".into())),
                None if only && !s.root => {}
                None => steps.push(s.clone()),
            }
        }
        let req = SettingsRequest {
            run_id: &cfg.run_id,
            packages: &eligible,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: false,
            full: inp.scope.targets.is_empty(),
            sudo: settings_sudo,
        };
        obs.settings_started(steps.len());
        let res = inp
            .settings
            .apply(&req, &steps, &mut |r: SettingStepResult| {
                obs.setting_done(&r);
                run.journal(Entry {
                    op: format!("setting-{}", r.step.action),
                    pkg: r.step.package.clone(),
                    provider: Some(r.step.kind.clone()),
                    from: None,
                    to: Some(r.step.id.clone()),
                    exit: i32::from(!r.ok),
                    dur_ms: r.dur_ms,
                    log: None,
                });
                if !r.ok {
                    run.errors.push(Diagnostic::from_code(
                        "setting-failed",
                        &[
                            ("setting", &r.step.label),
                            ("detail", r.detail.as_deref().unwrap_or("the change failed")),
                        ],
                    ));
                }
                settings_done.push(r);
            });
        if let Err(e) = res {
            run.errors.push(Diagnostic::from_code(
                "plan-failed",
                &[("package", "settings"), ("detail", &e)],
            ));
        }
        // Restarts are aggregated over the whole run and happen once, after the last change;
        // so does the "log out to apply" item.
        let mut apps: Vec<String> = Vec::new();
        for r in settings_done.iter().filter(|r| r.ok) {
            let named = r.step.restart.iter().map(String::as_str);
            let required = r
                .step
                .requires
                .iter()
                .filter_map(|q| q.strip_prefix("restart-app:"));
            for app in named.chain(required) {
                if !apps.iter().any(|a| a == app) {
                    apps.push(app.to_string());
                }
            }
            for need in r
                .step
                .requires
                .iter()
                .filter(|q| matches!(q.as_str(), "logout" | "reboot"))
            {
                let reason = dots_settings::todo_reason(need, &r.step.label);
                if !run
                    .state
                    .todos
                    .iter()
                    .any(|t| t.package == r.step.package && t.reason == reason)
                {
                    let t = Todo {
                        package: r.step.package.clone(),
                        reason,
                        run: cfg.run_id.clone(),
                    };
                    run.state.todos.push(t.clone());
                    run.todos.push(t);
                }
            }
        }
        run.save();
        if !apps.is_empty() {
            restarts_done = inp.settings.restart(&req, &apps);
            for r in restarts_done.iter().filter(|r| !r.ok) {
                run.warnings.push(Diagnostic::from_code(
                    "restart-failed",
                    &[
                        ("app", &r.app),
                        (
                            "detail",
                            r.detail.as_deref().unwrap_or("it did not restart"),
                        ),
                    ],
                ));
            }
        }
    }

    // Services: after config and settings are in place. A service whose config did not land
    // is left alone.
    let mut services_skipped: Vec<(ServiceStep, String)> = Vec::new();
    if !plan_v.services.is_empty() && !interrupted && !only {
        let bad_cfg: BTreeSet<&str> = files_done
            .iter()
            .filter(|f| !f.ok)
            .map(|f| f.step.package.as_str())
            .chain(files_skipped.iter().map(|(f, _)| f.package.as_str()))
            .collect();
        let bad_settings: BTreeSet<&str> = settings_done
            .iter()
            .filter(|s| !s.ok)
            .map(|s| s.step.package.as_str())
            .collect();
        let mut steps: Vec<ServiceStep> = Vec::new();
        for s in &plan_v.services {
            if s.teardown && !blocked.contains_key(s.package.as_str()) {
                continue; // already ran, before the files phase
            }
            if let Some(dep) = blocked.get(s.package.as_str()) {
                services_skipped.push((s.clone(), (*dep).to_string()));
            } else if bad_cfg.contains(s.package.as_str())
                || bad_settings.contains(s.package.as_str())
            {
                services_skipped.push((s.clone(), s.package.clone()));
            } else {
                steps.push(s.clone());
            }
        }
        let changed: Vec<String> = files_done
            .iter()
            .filter(|f| f.ok)
            .map(|f| f.step.package.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let req = ServicesRequest {
            run_id: &cfg.run_id,
            packages: &eligible,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: false,
            changed: &changed,
            teardown: false,
        };
        obs.services_started(steps.len());
        apply_services(inp, obs, &mut run, &req, &steps, &mut services_done);
    }

    // Hooks, last: `post_install` then `post_files` of each package (plan order), once its
    // package, its files and its earlier hooks went through.
    if !interrupted {
        let bad_files: BTreeSet<&str> = files_done
            .iter()
            .filter(|f| !f.ok)
            .map(|f| f.step.package.as_str())
            .chain(files_skipped.iter().map(|(f, _)| f.package.as_str()))
            .collect();
        let post_skip = |h: &HookStep, run: &Run<'_>| -> Option<String> {
            if let Some(why) = pre_skip(h, run) {
                Some(why)
            } else if h.event == "post_files" && bad_files.contains(h.package.as_str()) {
                Some("its files did not apply".into())
            } else {
                None
            }
        };
        let events: &[&str] = if only {
            &["post_install"]
        } else {
            &["post_install", "post_files"]
        };
        run_hooks(&mut run, events, &post_skip);
    }

    let failed = run.results.values().any(|r| r.status == StepStatus::Failed)
        || shims_done.iter().any(|s| !s.ok)
        || files_done.iter().any(|f| !f.ok)
        || run.hook_results.iter().any(|h| !h.ok)
        || settings_done.iter().any(|s| !s.ok)
        || services_done.iter().any(|s| !s.ok)
        || !run.errors.is_empty();
    let status = if interrupted {
        Status::Interrupted
    } else if failed {
        Status::Failed
    } else {
        Status::Done
    };
    // What the sudo policy left undone, for the end-of-run warning.
    let mut sudo_skipped: Vec<SudoItem> = Vec::new();
    for s in &plan_v.steps {
        let Some(r) = run.results.get(&s.package) else {
            continue;
        };
        let reason = match &r.status {
            StepStatus::SkippedSudo { reason } => reason.clone(),
            StepStatus::SkippedDueToDep { dependency } if listed.contains(&s.batch) => {
                format!("depends on {dependency}")
            }
            _ => continue,
        };
        sudo_skipped.push(SudoItem {
            name: s.package.clone(),
            what: "package",
            kind: s.provider.clone(),
            marker: if s.kind == crate::plan::StepKind::Install {
                "+"
            } else {
                "~"
            },
            reason,
        });
    }
    sudo_skipped.append(&mut sudo_skipped_settings);
    let sudo_unselected = if only {
        run.results
            .values()
            .filter(|r| r.status == StepStatus::NotSelected)
            .count()
            + plan_v.files.len()
            + plan_v.settings.iter().filter(|s| !s.root).count()
            + plan_v.services.len()
    } else {
        0
    };
    let steps: Vec<StepResult> = plan_v
        .steps
        .iter()
        .filter_map(|s| run.results.remove(&s.package))
        .collect();
    let Run {
        errors,
        warnings,
        todos,
        lock_updates,
        lock_drops,
        removals,
        hook_results,
        hooks_skipped,
        ..
    } = run;
    ApplyReport {
        status,
        run_id: cfg.run_id.clone(),
        plan: plan_v,
        steps,
        files: files_done,
        files_skipped,
        settings: settings_done,
        settings_skipped,
        restarts: restarts_done,
        services: services_done,
        services_skipped,
        shims: shims_done,
        shims_skipped,
        hooks: hook_results,
        hooks_skipped,
        todos,
        errors,
        warnings,
        lock_updates,
        lock_drops,
        removals,
        sudo_policy: cfg.sudo_policy,
        sudo_skipped,
        sudo_unselected,
    }
}

/// Apply `steps` through the files phase: record ownership, journal each one, turn failures
/// into errors and collect the results into `files_done`.
fn apply_file_steps(
    inp: &Inputs<'_>,
    cfg: &ApplyConfig,
    obs: &dyn Observer,
    run: &mut Run<'_>,
    packages: &[String],
    steps: &[FileStep],
    files_done: &mut Vec<FileStepResult>,
) {
    let req = FilesRequest {
        run_id: &cfg.run_id,
        packages,
        home: &inp.dirs.home,
        state_dir: &inp.dirs.state,
        dry_run: false,
    };
    obs.files_started(steps.len());
    let res = inp.files.apply(&req, steps, &mut |r: FileStepResult| {
        obs.file_done(&r);
        if r.ok && r.step.action == "remove" {
            run.state.files.remove(&r.step.dst);
            run.save();
        } else if r.ok {
            run.state.files.insert(
                r.step.dst.clone(),
                OwnedFile {
                    package: r.step.package.clone(),
                    kind: r.step.kind.clone(),
                    run: cfg.run_id.clone(),
                    ts: rfc3339(SystemTime::now()),
                },
            );
            run.save();
        }
        run.journal(Entry {
            op: r.step.kind.clone(),
            pkg: r.step.package.clone(),
            provider: None,
            from: None,
            to: Some(r.step.dst.clone()),
            exit: i32::from(!r.ok),
            dur_ms: r.dur_ms,
            log: None,
        });
        if !r.ok {
            run.errors.push(Diagnostic::from_code(
                "write-failed",
                &[
                    ("path", &r.step.dst),
                    (
                        "detail",
                        r.detail.as_deref().unwrap_or("file action failed"),
                    ),
                ],
            ));
        }
        files_done.push(r);
    });
    if let Err(e) = res {
        run.errors.push(Diagnostic::from_code(
            "plan-failed",
            &[("package", "files"), ("detail", &e)],
        ));
    }
}

/// Remove the old provider's copy after a switch (dots installed it, nothing else needs it) and
/// queue the old provider's lock rows for deletion. A failed removal leaves a todo.
fn apply_switch(
    inp: &Inputs<'_>,
    ctx: &Ctx,
    cfg: &ApplyConfig,
    sudo_ok: bool,
    run: &mut Run<'_>,
    package: &str,
    sw: &crate::plan::Switch,
) {
    for platform in &sw.lock_rows {
        run.lock_drops.push(LockDrop {
            package: package.to_string(),
            provider: sw.provider.clone(),
            platform: platform.clone(),
        });
    }
    if !sw.has_copy() {
        return;
    }
    let mut result = RemovalResult {
        package: package.to_string(),
        provider: sw.provider.clone(),
        native: sw.native.clone(),
        version: sw.version.clone(),
        removed: false,
        detail: sw.keep.clone(),
    };
    let provider = inp.registry.get_for(&sw.provider, package);
    if let (None, Some(provider)) = (&sw.keep, provider) {
        let mut failure: Option<String> = None;
        // Reverse dependencies the package manager would take along: leave those alone.
        for a in sw
            .remove
            .iter()
            .filter(|a| a.kind == dots_providers::OpKind::Remove)
        {
            match provider.removal_collateral(ctx, a) {
                Ok(others) if others.is_empty() => {}
                Ok(others) => {
                    failure = Some(format!(
                        "removing it would also remove {}; left in place",
                        others.join(", ")
                    ));
                    break;
                }
                Err(e) => {
                    failure = Some(format!(
                        "could not check what removing it would take along: {e}"
                    ));
                    break;
                }
            }
        }
        if failure.is_none() {
            for a in &sw.remove {
                let (out, _) = exec_action(provider, ctx, a, cfg.sudo, sudo_ok);
                if !out.ok {
                    failure = Some(out.error.unwrap_or_else(|| "the removal failed".into()));
                    break;
                }
            }
        }
        match failure {
            None => {
                result.removed = true;
                result.detail = None;
                run.journal(Entry {
                    op: "remove".into(),
                    pkg: package.to_string(),
                    provider: Some(sw.provider.clone()),
                    from: sw.version.clone(),
                    to: None,
                    exit: 0,
                    dur_ms: 0,
                    log: None,
                });
            }
            Some(why) => {
                run.journal(Entry {
                    op: "remove".into(),
                    pkg: package.to_string(),
                    provider: Some(sw.provider.clone()),
                    from: sw.version.clone(),
                    to: None,
                    exit: 1,
                    dur_ms: 0,
                    log: None,
                });
                result.detail = Some(why);
            }
        }
    }
    if let (false, Some(why), None) = (result.removed, &result.detail, &sw.keep) {
        run.warnings.push(Diagnostic::from_code(
            "action-failed",
            &[
                ("package", package),
                ("action", "remove"),
                ("provider", &sw.provider),
                (
                    "detail",
                    &format!("the old `{}` copy stays: {why}", sw.provider),
                ),
                ("log", "(no log)"),
                ("timeout", "its limit"),
            ],
        ));
        let t = Todo {
            package: package.to_string(),
            reason: format!("remove the old `{}` copy ({why})", sw.provider),
            run: cfg.run_id.clone(),
        };
        run.state.todos.retain(|x| x.package != t.package);
        run.state.todos.push(t.clone());
        run.todos.push(t);
        run.save();
    }
    run.removals.push(result);
}

/// Create, repair or remove one shim and update the ledger. `Err` is the reason it failed.
fn apply_shim(
    inp: &Inputs<'_>,
    run: &mut Run<'_>,
    st: &ShimStep,
    path_var: &str,
) -> Result<(), String> {
    let key = st.path.to_string_lossy().into_owned();
    if st.action == "remove" {
        if matches!(
            shims::state_of(&st.path),
            shims::ShimState::Ok | shims::ShimState::Dangling
        ) {
            std::fs::remove_file(&st.path)
                .map_err(|e| format!("cannot remove {}: {e}", st.path.display()))?;
        }
        run.state.shims.remove(&key);
        run.save();
        return Ok(());
    }
    // The install may just have put the target on disk: resolve it now.
    let target = shims::find_bin(
        &st.bin,
        path_var,
        &inp.dirs.home,
        &shims::bin_dir(&inp.dirs.home),
    )
    .ok_or_else(|| {
        format!(
            "`{}` (the target of shim `{}`) was not found on PATH",
            st.bin, st.name
        )
    })?;
    shims::make_link(&st.path, &target)?;
    run.state.shims.insert(
        key,
        shims::OwnedShim {
            package: st.package.clone(),
            target: target.to_string_lossy().into_owned(),
            run: run.run_id.clone(),
            ts: rfc3339(SystemTime::now()),
        },
    );
    run.save();
    Ok(())
}

/// Run `steps` through the services phase, journaling each and turning failures into errors.
fn apply_services(
    inp: &Inputs<'_>,
    obs: &dyn Observer,
    run: &mut Run<'_>,
    req: &ServicesRequest<'_>,
    steps: &[ServiceStep],
    done_out: &mut Vec<ServiceStepResult>,
) {
    let res = inp.services.apply(req, steps, &mut |r: ServiceStepResult| {
        obs.service_done(&r);
        run.journal(Entry {
            op: format!("service-{}", r.step.action),
            pkg: r.step.package.clone(),
            provider: Some(r.step.manager.clone()),
            from: None,
            to: Some(r.step.name.clone()),
            exit: i32::from(!r.ok),
            dur_ms: r.dur_ms,
            log: None,
        });
        if !r.ok {
            run.errors.push(Diagnostic::from_code(
                "service-failed",
                &[
                    ("service", &r.step.name),
                    ("action", &r.step.action),
                    ("detail", r.detail.as_deref().unwrap_or("the action failed")),
                ],
            ));
        }
        done_out.push(r);
    });
    if let Err(e) = res {
        run.errors.push(Diagnostic::from_code(
            "plan-failed",
            &[("package", "services"), ("detail", &e)],
        ));
    }
}

/// Close the manual actions (`logout` / `reboot`) whose setting now reads as declared. `true`
/// when `state` changed.
fn close_settled_todos(inp: &Inputs<'_>, req: &SettingsRequest<'_>, state: &mut State) -> bool {
    if state.todos.is_empty() {
        return false;
    }
    let done = inp.settings.settled(req);
    let before = state.todos.len();
    state
        .todos
        .retain(|t| !done.iter().any(|(p, r)| *p == t.package && *r == t.reason));
    state.todos.len() != before
}

/// An adopted package's `post_install` never ran (dots did not install it): record it as
/// pending so it survives an interrupted run, a `--files-only` run or a failure.
fn mark_adopted_hook(inp: &Inputs<'_>, package: &str, state: &mut State) -> bool {
    inp.repo
        .packages
        .get(package)
        .is_some_and(|p| p.hooks.post_install.is_some())
        && state
            .hooks_pending
            .insert(format!("{package}:post_install"))
}

fn adopt_entry(u: &crate::locks::LockUpdate) -> Entry {
    Entry {
        op: "adopt".into(),
        pkg: u.package.clone(),
        provider: Some(u.provider.clone()),
        from: None,
        to: u.version.clone().or_else(|| u.rev.clone()),
        exit: 0,
        dur_ms: 0,
        log: None,
    }
}
