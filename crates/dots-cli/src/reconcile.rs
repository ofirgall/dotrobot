//! `dots plan` and `dots apply`: wire the engine to the real machine.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dots_core::{Diagnostic, ExitCode, Report};
use dots_engine::{
    ApplyConfig, ApplyReport, Inputs, JournalMeta, Mode, Observer, Prompter, RunLock, Scope,
    Status, StepResult, apply_observed, new_run_id, plan, plan_json,
};
use dots_resolve::resolve_full;

use crate::app::Context;
use crate::context::{RepoCtx, load_repo};
use crate::files_phase::RepoFiles;
use crate::providers;
use crate::services_phase::RepoServices;
use crate::settings_phase::RepoSettings;

/// The files phase: `dots-files` over the loaded repo.
fn files_phase<'a>(p: &'a Prepared, dirs: &'a dots_core::Dirs) -> RepoFiles<'a> {
    RepoFiles {
        dirs,
        rc: &p.rc,
        resolution: &p.resolution,
    }
}

/// The settings phase: `dots-settings` over the loaded repo.
fn settings_phase<'a>(p: &'a Prepared, ctx: &'a dots_providers::Ctx) -> RepoSettings<'a> {
    RepoSettings {
        rc: &p.rc,
        resolution: &p.resolution,
        ctx,
    }
}

/// The hooks phase: the script runtime over the loaded repo.
fn hooks_phase<'a>(
    p: &'a Prepared,
    dirs: &'a dots_core::Dirs,
    settings: &'a dots_config::Settings,
) -> crate::hooks_phase::RepoHooks<'a> {
    crate::hooks_phase::RepoHooks {
        facts: &p.rc.m.facts,
        dirs,
        root: &p.rc.m.root,
        tags: &p.rc.m.machine.tags,
        settings,
    }
}

/// The services phase: `dots-services` over the loaded repo.
fn services_phase<'a>(p: &'a Prepared, ctx: &'a dots_providers::Ctx) -> RepoServices<'a> {
    RepoServices {
        rc: &p.rc,
        resolution: &p.resolution,
        ctx,
    }
}

pub struct Args {
    pub targets: Vec<String>,
    pub files_only: bool,
    pub packages_only: bool,
}

impl Args {
    fn scope(&self) -> Scope {
        Scope {
            targets: self.targets.clone(),
            files_only: self.files_only,
            packages_only: self.packages_only,
        }
    }
}

fn failure(
    cmd: &str,
    exit: ExitCode,
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
) -> Report {
    let mut r = Report::failure(cmd, exit, errors);
    r.warnings = warnings;
    r
}

/// Split resolution diagnostics: a package nothing can install is a warning (the plan lists it
/// as not planned), anything else is an error.
fn split(rc: &RepoCtx, res: &dots_resolve::Resolution) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
    let mut errors = Vec::new();
    let mut warnings = rc.m.warnings.clone();
    for d in &res.errors {
        if d.code == "no-provider-satisfies" {
            warnings.push(d.clone());
        } else {
            errors.push(d.clone());
        }
    }
    warnings.extend(res.warnings.iter().cloned());
    // Invalid setting values and bundle conflicts stop a run; stale values only warn.
    let settings = crate::config_cmd::settings_for(rc);
    errors.extend(settings.errors);
    warnings.extend(settings.warnings);
    (errors, warnings)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(crate) fn journal_meta(rc: &RepoCtx) -> JournalMeta {
    JournalMeta {
        host: rc.m.facts.hostname.clone(),
        root: "main".into(),
        repo_rev: git(&rc.m.root, &["rev-parse", "--short=8", "HEAD"]),
        repo_dirty: git(&rc.m.root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()),
        upstream_rev: None,
        dots_version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Everything both commands compute before the engine: the loaded repo, the registry of real
/// providers, installed state and the resolution.
pub(crate) struct Prepared {
    pub(crate) rc: RepoCtx,
    pub(crate) registry: dots_providers::Registry,
    pub(crate) installed: dots_resolve::InstalledMap,
    pub(crate) resolution: dots_resolve::Resolution,
    pub(crate) pctx: dots_providers::Ctx,
    /// `dots.lock`, and this machine's platform key in it.
    pub(crate) lock: dots_lock::Lock,
    pub(crate) machine_key: String,
    /// The script provider instances, for the hashes of what they downloaded.
    pub(crate) scripts: providers::ScriptHandles,
}

pub(crate) fn prepare(
    ctx: &Context,
    root: Option<&Path>,
    run_id: &str,
) -> Result<Prepared, (Vec<Diagnostic>, Vec<Diagnostic>)> {
    let rc = load_repo(ctx, root)?;
    let pctx = providers::with_env_path(
        providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, run_id),
        &rc,
        &ctx.dirs,
    );
    let settings = crate::config_cmd::settings_for(&rc);
    let (registry, scripts) = providers::registry_with_handles(
        &ctx.dirs,
        &pctx,
        &rc.m.facts,
        Some(&rc.repo),
        &rc.m.root,
        providers::ScriptInputs {
            tags: &rc.m.machine.tags,
            settings: &settings,
        },
    );
    let installed = providers::installed(&registry, &pctx);
    let lock = dots_lock::Lock::load(&crate::lock_cmd::path(&rc.m.root))
        .map_err(|d| (vec![d], rc.m.warnings.clone()))?;
    let machine_key = crate::lock_cmd::machine_key(&rc);
    // Provider choice is constraint-aware: ask what providers offer when a constraint needs it.
    let resolution = {
        let offers = providers::LiveOffers::new(&registry, &pctx);
        resolve_full(
            &dots_resolve::Inputs {
                repo: &rc.repo,
                machine: &rc.m.machine,
                facts: &rc.m.facts,
                chain: &rc.chain,
                providers: &registry,
                installed: &installed,
            },
            &offers,
            &settings,
        )
    };
    Ok(Prepared {
        rc,
        registry,
        installed,
        resolution,
        pctx,
        lock,
        machine_key,
        scripts,
    })
}

// ------------------------------------------------------------------------------ plan

pub fn plan_cmd(ctx: &Context, args: &Args, check: bool) -> Report {
    let run_id = new_run_id();
    let p = match prepare(ctx, None, &run_id) {
        Ok(p) => p,
        Err((e, w)) => return failure("plan", ExitCode::Error, e, w),
    };
    let (errors, warnings) = split(&p.rc, &p.resolution);
    if !errors.is_empty() {
        return failure("plan", ExitCode::Error, errors, warnings);
    }
    let files = files_phase(&p, &ctx.dirs);
    let settings = settings_phase(&p, &p.pctx);
    let services = services_phase(&p, &p.pctx);
    let hook_settings = crate::config_cmd::settings_for(&p.rc);
    let hooks = hooks_phase(&p, &ctx.dirs, &hook_settings);
    let scope = args.scope();
    let unreviewed = BTreeSet::new();
    let offers = providers::LiveOffers::new(&p.registry, &p.pctx);
    let no_upgrade = BTreeMap::new();
    let locks = dots_engine::LockInputs {
        lock: &p.lock,
        machine_key: &p.machine_key,
        offers: &offers,
        upgrade: &no_upgrade,
    };
    let inp = Inputs {
        repo: &p.rc.repo,
        resolution: &p.resolution,
        registry: &p.registry,
        installed: &p.installed,
        files: &files,
        settings: &settings,
        services: &services,
        hooks: &hooks,
        ctx: &p.pctx,
        dirs: &ctx.dirs,
        scope: &scope,
        unreviewed: &unreviewed,
        locks: Some(&locks),
    };
    match plan(&inp, &run_id) {
        Err(errors) => failure("plan", ExitCode::Error, errors, warnings),
        Ok(pl) => {
            let mut r = crate::views::plan_report(
                Report::ok("plan", plan_json(&pl)),
                &pl,
                &backup_dir(ctx),
            );
            r.warnings = warnings;
            if check && !pl.is_empty() {
                r.exit = ExitCode::PlanNonEmpty;
            }
            r
        }
    }
}

// ------------------------------------------------------------------------------ apply

pub struct ApplyArgs {
    pub common: Args,
    pub yes: bool,
    pub dry_run: bool,
    pub allow_downgrade: bool,
    pub root: Option<PathBuf>,
    /// What to do with the steps that need sudo.
    pub sudo: dots_engine::SudoPolicy,
}

/// Asks on the terminal. Never constructed as interactive under `--no-input`, `--json` or a
/// non-TTY stdin/stderr.
struct Tty {
    interactive: bool,
}

impl Prompter for Tty {
    fn can_prompt(&self) -> bool {
        self.interactive
    }

    fn confirm(&self, text: &str) -> bool {
        let mut err = std::io::stderr();
        let _ = writeln!(err, "{text}");
        let _ = write!(err, "Proceed? [y/N] ");
        let _ = err.flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).is_ok()
            && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

/// Set by `apply`; the process-wide SIGINT handler (see `main`) sets it instead of exiting.
pub static STOP: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

/// Called by the SIGINT handler: the first Ctrl-C during `apply` asks it to stop after the
/// current action; any other Ctrl-C (or a second one) ends the process with 130.
pub fn on_interrupt() {
    if let Some(f) = STOP.get()
        && !f.swap(true, Ordering::SeqCst)
    {
        return;
    }
    std::process::exit(ExitCode::Interrupted.code());
}

fn stop_flag() -> Arc<AtomicBool> {
    STOP.get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

pub fn apply_cmd(ctx: &Context, a: &ApplyArgs) -> Report {
    apply_inner(ctx, a, "apply", None).0
}

/// Packages `dots upgrade` moves, computed from the prepared run (or the report to return
/// instead: an error, or "nothing to upgrade").
pub(crate) type UpgradeSet = BTreeMap<String, dots_engine::UpgradeTarget>;
pub(crate) type Planner<'a> = &'a dyn Fn(&Prepared) -> Result<UpgradeSet, Box<Report>>;

/// `apply`, or (with a `planner`) `upgrade`: the same engine run, with some packages moved to
/// explicit targets. Returns the report and the lock rows that were written.
pub(crate) fn apply_inner(
    ctx: &Context,
    a: &ApplyArgs,
    command: &str,
    planner: Option<Planner<'_>>,
) -> (Report, Vec<crate::lock_cmd::Written>) {
    let none = |r: Report| (r, Vec::new());
    let run_id = new_run_id();
    let _ = ctx.dirs.ensure_all();
    // Only a real run takes the lock; `--dry-run` is `plan`.
    let lock = if a.dry_run {
        None
    } else {
        match RunLock::acquire(&ctx.dirs.run_lock()) {
            Ok(l) => Some(l),
            Err(d) => {
                let exit = if d.code == "lock-busy" {
                    ExitCode::LockBusy
                } else {
                    ExitCode::Error
                };
                return none(failure(command, exit, vec![d], vec![]));
            }
        }
    };

    let p = match prepare(ctx, a.root.as_deref(), &run_id) {
        Ok(p) => p,
        Err((e, w)) => return none(failure(command, ExitCode::Error, e, w)),
    };
    let (errors, warnings) = split(&p.rc, &p.resolution);
    if !errors.is_empty() {
        return none(failure(command, ExitCode::Error, errors, warnings));
    }
    let files = files_phase(&p, &ctx.dirs);
    let upgrade = match planner {
        Some(f) => match f(&p) {
            Ok(set) => set,
            Err(report) => return none(*report),
        },
        None => BTreeMap::new(),
    };
    let scope = if planner.is_some() {
        Scope {
            targets: upgrade.keys().cloned().collect(),
            files_only: false,
            packages_only: true,
        }
    } else {
        a.common.scope()
    };
    let unreviewed = BTreeSet::new();
    let pctx = providers::with_env_path(
        providers::exec_ctx(&ctx.dirs, ctx.opts.offline, a.dry_run, &run_id),
        &p.rc,
        &ctx.dirs,
    );
    let settings = settings_phase(&p, &pctx);
    let services = services_phase(&p, &pctx);
    let hook_settings = crate::config_cmd::settings_for(&p.rc);
    let hooks = hooks_phase(&p, &ctx.dirs, &hook_settings);
    let offers = providers::LiveOffers::new(&p.registry, &pctx);
    let locks = dots_engine::LockInputs {
        lock: &p.lock,
        machine_key: &p.machine_key,
        offers: &offers,
        upgrade: &upgrade,
    };
    let inp = Inputs {
        repo: &p.rc.repo,
        resolution: &p.resolution,
        registry: &p.registry,
        installed: &p.installed,
        files: &files,
        settings: &settings,
        services: &services,
        hooks: &hooks,
        ctx: &pctx,
        dirs: &ctx.dirs,
        scope: &scope,
        unreviewed: &unreviewed,
        locks: Some(&locks),
    };
    let cfg = ApplyConfig {
        run_id: run_id.clone(),
        yes: a.yes,
        allow_downgrade: a.allow_downgrade,
        // Running as root: sudo steps are ordinary, nothing is primed or skipped.
        sudo: p.rc.m.facts.user != "root" && !crate::settings_phase::running_as_root(),
        sudo_policy: a.sudo,
        journal: journal_meta(&p.rc),
        stop: if a.dry_run {
            Arc::new(AtomicBool::new(false))
        } else {
            stop_flag()
        },
    };
    let interactive = !ctx.opts.no_input
        && !ctx.opts.json
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal();
    let mode = match &lock {
        Some(l) => Mode::Live(l),
        None => Mode::DryRun,
    };
    let ui = dots_ui::Ui::from_process(&ctx.opts, &dots_core::ProcessEnv);
    let progress = (!ctx.opts.json && !ctx.opts.quiet && !a.dry_run && ui.stderr_tty)
        .then(|| LiveProgress::new(&ui));
    let mut env = dots_env::EnvCtx::new(&ctx.dirs.home);
    env.vars = dots_files::resolve_path_vars(&p.rc.repo.manifest.paths, p.rc.chain.levels());
    let report = apply_observed(
        &inp,
        &cfg,
        &Tty { interactive },
        mode,
        progress
            .as_ref()
            .map_or(&dots_engine::NoObserver as &dyn Observer, |p| p),
        Some(env),
    );
    drop(progress);
    let mut warnings = warnings;
    let hashes = crate::lock_cmd::first_use_hashes(&p.scripts, &p.rc.repo, &p.lock, &report);
    let written = match crate::lock_cmd::write_updates(
        &p.rc.m.root,
        &report.lock_updates,
        &report.lock_drops,
        &hashes,
    ) {
        Ok(w) => w,
        Err(d) => {
            warnings.push(d);
            Vec::new()
        }
    };
    (
        to_report(command, report, warnings, &backup_dir(ctx)),
        written,
    )
}

fn backup_dir(ctx: &Context) -> String {
    format!("{}/backup/<run-id>/", ctx.dirs.state.display())
}

fn to_report(
    command: &str,
    r: ApplyReport,
    mut warnings: Vec<Diagnostic>,
    backup_dir: &str,
) -> Report {
    let exit = match r.status {
        Status::Done | Status::DryRun => ExitCode::Success,
        Status::Interrupted => ExitCode::Interrupted,
        Status::Failed | Status::ConfirmationRequired | Status::Declined | Status::PlanFailed => {
            ExitCode::Error
        }
    };
    warnings.extend(r.warnings.iter().cloned());
    let mut report = crate::views::apply_report(Report::ok(command, r.to_json()), &r, backup_dir);
    report.exit = exit;
    report.errors = r.errors.clone();
    report.warnings = warnings;
    report
}

/// Live progress on stderr, only when stderr is a terminal (never under `--json`, `--quiet`
/// or a pipe, so captured output stays stable).
struct LiveProgress {
    p: dots_ui::Progress,
    batches: std::sync::Mutex<std::collections::BTreeMap<usize, dots_ui::TaskId>>,
}

impl LiveProgress {
    fn new(ui: &dots_ui::Ui) -> Self {
        LiveProgress {
            p: dots_ui::Progress::stderr(ui),
            batches: Default::default(),
        }
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        self.p.finish_all();
    }
}

impl Observer for LiveProgress {
    fn planned(&self, plan: &dots_engine::Plan) {
        self.p.set_total(
            plan.steps.len()
                + plan.files.len()
                + plan.settings.len()
                + plan.services.len()
                + plan.shims.len()
                + plan.hooks.len(),
        );
    }

    fn sudo_prompt(&self, begin: bool) {
        if begin {
            self.p.suspend();
        } else {
            self.p.resume();
        }
    }

    fn batch_started(&self, b: &dots_engine::Batch) {
        let id = self.p.start(&b.packages.join(", "), &b.provider);
        self.batches.lock().unwrap().insert(b.id, id);
    }

    fn batch_finished(&self, b: &dots_engine::Batch, results: &[StepResult]) {
        use dots_core::State;
        use dots_engine::StepStatus;
        let Some(id) = self.batches.lock().unwrap().remove(&b.id) else {
            return;
        };
        let (state, detail) = if results.iter().all(|r| r.status == StepStatus::Ok) {
            (State::Done, "installed".to_string())
        } else {
            (State::Failed, "see the summary below".to_string())
        };
        let dur = results.iter().map(|r| r.dur_ms).sum::<u64>();
        self.p.finish(
            id,
            state,
            &detail,
            Some(std::time::Duration::from_millis(dur)),
        );
    }

    fn file_done(&self, r: &dots_engine::FileStepResult) {
        use dots_core::State;
        let id = self.p.start(&r.step.dst, &r.step.kind);
        let (state, detail) = if r.ok {
            (State::Done, r.step.action.clone())
        } else {
            (State::Failed, r.detail.clone().unwrap_or_default())
        };
        self.p.finish(
            id,
            state,
            &detail,
            Some(std::time::Duration::from_millis(r.dur_ms)),
        );
    }

    fn setting_done(&self, r: &dots_engine::SettingStepResult) {
        use dots_core::State;
        let id = self.p.start(&r.step.label, &r.step.kind);
        let (state, detail) = if r.ok {
            // An ok step with a note was skipped (its tool is still missing).
            (
                State::Done,
                r.detail.clone().unwrap_or_else(|| r.step.detail.clone()),
            )
        } else {
            (State::Failed, r.detail.clone().unwrap_or_default())
        };
        self.p.finish(
            id,
            state,
            &detail,
            Some(std::time::Duration::from_millis(r.dur_ms)),
        );
    }

    fn shim_done(&self, r: &dots_engine::ShimStepResult) {
        use dots_core::State;
        let id = self.p.start(&r.step.dst, "shim");
        let (state, detail) = if r.ok {
            (State::Done, r.step.action.clone())
        } else {
            (State::Failed, r.detail.clone().unwrap_or_default())
        };
        self.p.finish(
            id,
            state,
            &detail,
            Some(std::time::Duration::from_millis(r.dur_ms)),
        );
    }

    fn hook_done(&self, r: &dots_engine::HookStepResult) {
        use dots_core::State;
        let id = self.p.start(&r.step.package, &r.step.event);
        let (state, detail) = if r.ok {
            (State::Done, r.step.short())
        } else {
            (State::Failed, r.detail.clone().unwrap_or_default())
        };
        self.p.finish(
            id,
            state,
            &detail,
            Some(std::time::Duration::from_millis(r.dur_ms)),
        );
    }

    fn service_done(&self, r: &dots_engine::ServiceStepResult) {
        use dots_core::State;
        let id = self.p.start(&r.step.name, &r.step.manager);
        let (state, detail) = if r.ok {
            (State::Done, r.step.action.clone())
        } else {
            (State::Failed, r.detail.clone().unwrap_or_default())
        };
        self.p.finish(
            id,
            state,
            &detail,
            Some(std::time::Duration::from_millis(r.dur_ms)),
        );
    }
}
