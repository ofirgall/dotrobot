//! `dots plan` and `dots apply`: wire the engine to the real machine.

use std::collections::BTreeSet;
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
use dots_resolve::resolve;

use crate::app::Context;
use crate::context::{RepoCtx, load_repo};
use crate::files_phase::RepoFiles;
use crate::providers;

/// The files phase: `dots-files` over the loaded repo.
fn files_phase<'a>(p: &'a Prepared, dirs: &'a dots_core::Dirs) -> RepoFiles<'a> {
    RepoFiles {
        dirs,
        rc: &p.rc,
        resolution: &p.resolution,
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

fn journal_meta(rc: &RepoCtx) -> JournalMeta {
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
struct Prepared {
    rc: RepoCtx,
    registry: dots_providers::Registry,
    installed: dots_resolve::InstalledMap,
    resolution: dots_resolve::Resolution,
    pctx: dots_providers::Ctx,
}

fn prepare(
    ctx: &Context,
    root: Option<&Path>,
    run_id: &str,
) -> Result<Prepared, (Vec<Diagnostic>, Vec<Diagnostic>)> {
    let rc = load_repo(ctx, root)?;
    let pctx = providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, run_id);
    let registry = providers::registry(&ctx.dirs, &pctx, &rc.m.facts, Some(&rc.repo), &rc.m.root);
    let installed = providers::installed(&registry, &pctx);
    let resolution = resolve(&dots_resolve::Inputs {
        repo: &rc.repo,
        machine: &rc.m.machine,
        facts: &rc.m.facts,
        chain: &rc.chain,
        providers: &registry,
        installed: &installed,
    });
    Ok(Prepared {
        rc,
        registry,
        installed,
        resolution,
        pctx,
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
    let scope = args.scope();
    let unreviewed = BTreeSet::new();
    let inp = Inputs {
        repo: &p.rc.repo,
        resolution: &p.resolution,
        registry: &p.registry,
        installed: &p.installed,
        files: &files,
        ctx: &p.pctx,
        dirs: &ctx.dirs,
        scope: &scope,
        unreviewed: &unreviewed,
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
                return failure("apply", exit, vec![d], vec![]);
            }
        }
    };

    let p = match prepare(ctx, a.root.as_deref(), &run_id) {
        Ok(p) => p,
        Err((e, w)) => return failure("apply", ExitCode::Error, e, w),
    };
    let (errors, warnings) = split(&p.rc, &p.resolution);
    if !errors.is_empty() {
        return failure("apply", ExitCode::Error, errors, warnings);
    }
    let files = files_phase(&p, &ctx.dirs);
    let scope = a.common.scope();
    let unreviewed = BTreeSet::new();
    let pctx = providers::exec_ctx(&ctx.dirs, ctx.opts.offline, a.dry_run, &run_id);
    let inp = Inputs {
        repo: &p.rc.repo,
        resolution: &p.resolution,
        registry: &p.registry,
        installed: &p.installed,
        files: &files,
        ctx: &pctx,
        dirs: &ctx.dirs,
        scope: &scope,
        unreviewed: &unreviewed,
    };
    let cfg = ApplyConfig {
        run_id: run_id.clone(),
        yes: a.yes,
        allow_downgrade: a.allow_downgrade,
        sudo: p.rc.m.facts.user != "root",
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
    to_report(report, warnings, &backup_dir(ctx))
}

fn backup_dir(ctx: &Context) -> String {
    format!("{}/backup/<run-id>/", ctx.dirs.state.display())
}

fn to_report(r: ApplyReport, mut warnings: Vec<Diagnostic>, backup_dir: &str) -> Report {
    let exit = match r.status {
        Status::Done | Status::DryRun => ExitCode::Success,
        Status::Interrupted => ExitCode::Interrupted,
        Status::Failed | Status::ConfirmationRequired | Status::Declined | Status::PlanFailed => {
            ExitCode::Error
        }
    };
    warnings.extend(r.warnings.iter().cloned());
    let mut report = crate::views::apply_report(Report::ok("apply", r.to_json()), &r, backup_dir);
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
        self.p.set_total(plan.steps.len() + plan.files.len());
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
}
