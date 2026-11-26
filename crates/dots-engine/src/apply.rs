//! `apply`: confirm, prime sudo once, run batches on a DAG scheduler, apply files, record.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use dots_core::Diagnostic;
use dots_providers::{Action, Cmd, Ctx, Installed, Provider, ProviderError, Stdin};

use crate::files::{FileStep, FileStepResult, FilesRequest};
use crate::journal::{Entry, Journal, JournalMeta, rfc3339};
use crate::lock::RunLock;
use crate::plan::{Batch, Inputs, PkgStep, Plan, plan, relative};
use crate::render::plan_text;
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

pub struct ApplyConfig {
    pub run_id: String,
    /// `--yes`.
    pub yes: bool,
    pub allow_downgrade: bool,
    /// Prime sudo once and prefix root actions with `sudo -n`. False when already root.
    pub sudo: bool,
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
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Failed => "failed",
            StepStatus::SkippedDueToDep { .. } => "skipped-due-to-dep",
            StepStatus::Deferred => "deferred",
            StepStatus::NotRun => "not-run",
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
    pub todos: Vec<Todo>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
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
            todos: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

// ------------------------------------------------------------------------------ batch worker

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

fn run_batch(
    provider: &dyn Provider,
    ctx: &Ctx,
    batch: &Batch,
    stop: &AtomicBool,
    use_sudo: bool,
    sudo_ok: bool,
) -> BatchOutcome {
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
        let started = Instant::now();
        let mut act: Action = action.clone();
        if act.needs_root && use_sudo {
            if !sudo_ok {
                actions.push(ActionOutcome {
                    natives,
                    ran: true,
                    ok: false,
                    log: None,
                    error: Some("could not get root access".into()),
                    code: Some("sudo-failed"),
                    dur_ms: 0,
                });
                if act.ops.is_empty() {
                    prep_failed = true;
                }
                continue;
            }
            let mut argv = vec![SUDO.to_string(), "-n".to_string()];
            argv.append(&mut act.argv);
            act.argv = argv;
        }
        let res = provider.exec(ctx, &act);
        let dur_ms = started.elapsed().as_millis() as u64;
        let out = match res {
            Ok(o) if o.success => ActionOutcome {
                natives,
                ran: true,
                ok: true,
                log: o.log,
                error: None,
                code: None,
                dur_ms,
            },
            Ok(o) => ActionOutcome {
                natives,
                ran: true,
                ok: false,
                error: Some(format!("`{}` failed", action.argv.join(" "))),
                log: o.log,
                code: None,
                dur_ms,
            },
            Err(e) => {
                let (code, log) = match &e {
                    ProviderError::Coded { code, log, .. } => (Some(*code), log.clone()),
                    _ => (None, None),
                };
                ActionOutcome {
                    natives,
                    ran: true,
                    ok: false,
                    error: Some(e.to_string()),
                    log,
                    code,
                    dur_ms,
                }
            }
        };
        if !out.ok && act.ops.is_empty() {
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
    }
}

// ------------------------------------------------------------------------------ the run

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
}

impl Run<'_> {
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

    fn finish_batch(&mut self, batch: &Batch, out: BatchOutcome) -> bool {
        let mut all_ok = true;
        for pkg in &batch.packages {
            let Some(step) = self.plan.step(pkg) else {
                continue;
            };
            let mine: Vec<&ActionOutcome> = out
                .actions
                .iter()
                .filter(|a| a.natives.contains(&step.native) || a.natives.is_empty())
                .collect();
            let mine: Vec<&ActionOutcome> = if mine.is_empty() {
                out.actions.iter().collect()
            } else {
                mine
            };
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
                    let mut d = Diagnostic::from_code(
                        code,
                        &[
                            ("package", pkg),
                            ("action", step.kind.as_str()),
                            ("provider", &step.provider),
                            ("detail", &detail),
                            ("log", &logs),
                            ("timeout", "its limit"),
                        ],
                    );
                    let tail = log_tail(&log);
                    if !tail.is_empty() {
                        d.hint = format!("{}\n{tail}", d.hint);
                    }
                    self.errors.push(d);
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
        return ApplyReport::bare(Status::Done, &cfg.run_id, plan_v);
    }

    // Confirmation policy.
    let always = plan_v.always_confirm(cfg.allow_downgrade);
    let ordinary = plan_v.touches_more_than_owned_files() && !cfg.yes;
    if ordinary || !always.is_empty() {
        if prompter.can_prompt() {
            if !prompter.confirm(&plan_text(&plan_v)) {
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
    };
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
    let can_prompt = prompter.can_prompt() && !cfg.yes;

    // One sudo prompt for the whole run.
    let needs_sudo = cfg.sudo && plan_v.needs_root();
    let sudo_ok = if needs_sudo {
        let cmd = Cmd::new(SUDO).arg("-v").stdin(Stdin::Inherit).timeout(None);
        obs.sudo_prompt(true);
        let ok = ctx.runner.run(&cmd).is_ok_and(|o| o.success());
        obs.sudo_prompt(false);
        ok
    } else {
        true
    };
    let (ka_stop_tx, ka_stop_rx) = mpsc::channel::<()>();

    let n = plan_v.batches.len();
    let mut status = vec![B::Pending; n];
    let blocked_by: BTreeMap<usize, usize> = BTreeMap::new();
    let _ = blocked_by;

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

        loop {
            // Dependents of a dead batch are skipped (to a fixpoint).
            loop {
                let mut changed = false;
                for b in &plan_v.batches {
                    if status[b.id] != B::Pending {
                        continue;
                    }
                    let dead = b.deps.iter().find(|d| status[**d] == B::Dead).copied();
                    if let Some(d) = dead {
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
                                run.warnings.push(Diagnostic::from_code(
                                    "skipped-due-to-dep",
                                    &[("package", p), ("dependency", &dep_pkg)],
                                ));
                            }
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
                    status[b.id] = B::Running;
                    running += 1;
                    obs.batch_started(b);
                    if provider.caps().exclusive_lock {
                        running_providers.insert(b.provider.clone());
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
                    scope.spawn(move || {
                        let out = run_batch(provider, &bctx, b, stop, use_sudo, sudo_ok);
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
            if inp
                .registry
                .get_for(&b.provider, &b.packages[0])
                .is_some_and(|p| p.caps().exclusive_lock)
            {
                running_providers.remove(&b.provider);
            }
            let ok = run.finish_batch(b, out);
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

    // Steps never reached.
    for s in &plan_v.steps {
        if !run.results.contains_key(&s.package) {
            run.mark(s, StepStatus::NotRun);
        }
    }

    // Files phase: only for packages whose install went through.
    let interrupted = stop.load(Ordering::SeqCst);
    let mut files_done: Vec<FileStepResult> = Vec::new();
    let mut files_skipped: Vec<(FileStep, String)> = Vec::new();
    if !plan_v.files.is_empty() && !interrupted {
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
        let mut steps: Vec<FileStep> = Vec::new();
        for f in &plan_v.files {
            match blocked.get(f.package.as_str()) {
                Some(dep) => files_skipped.push((f.clone(), (*dep).to_string())),
                None => steps.push(f.clone()),
            }
        }
        let req = FilesRequest {
            run_id: &cfg.run_id,
            packages: &eligible,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: false,
        };
        obs.files_started(steps.len());
        let res = inp.files.apply(&req, &steps, &mut |r: FileStepResult| {
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

    let failed = run.results.values().any(|r| r.status == StepStatus::Failed)
        || files_done.iter().any(|f| !f.ok)
        || !run.errors.is_empty();
    let status = if interrupted {
        Status::Interrupted
    } else if failed {
        Status::Failed
    } else {
        Status::Done
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
        ..
    } = run;
    ApplyReport {
        status,
        run_id: cfg.run_id.clone(),
        plan: plan_v,
        steps,
        files: files_done,
        files_skipped,
        todos,
        errors,
        warnings,
    }
}
