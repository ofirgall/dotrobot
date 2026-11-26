//! Package hooks: `[hooks] pre_install | post_install | pre_files | post_files`
//! (ARCHITECTURE section 3 "Hooks", 14.1 pipeline order; specs `reconcile-engine` "Action
//! ordering" step 6 and `provider-contract` "Plugin manager shapes").
//!
//! Semantics, where the specs are silent, chosen to be the simplest thing consistent with how
//! the script provider runs commands:
//!
//! - **When each fires.** A hook fires only when its trigger changed in this run (plan time):
//!   `pre_install`/`post_install` when the package has an install, upgrade or downgrade step;
//!   `pre_files`/`post_files` when the package has at least one file step (a step that changes
//!   nothing is never planned, so an unchanged package never runs its hooks). A hook that
//!   failed in an earlier run fires again until it passes (the trigger is no longer "changed"
//!   by then, so without this a failed hook would never be retried).
//! - **Where in the pipeline.** `pre_install` runs right before the package's provider batch
//!   (inside the batch worker, so after the package's dependencies are installed);
//!   `pre_files` right before the files phase; `post_install` then `post_files` in the hooks
//!   slot after services (Files, Settings, Services, Hooks, Env, Todos). `--files-only` runs
//!   only the files hooks, `--packages-only` only the install hooks, `--dry-run` none (the
//!   plan lists them).
//! - **How they run.** Through the [`HooksPhase`]: the same runtime contract as a script
//!   command (the shell, the package directory as working directory, the `DOTS_*` variables,
//!   stdin at `/dev/null`, a timeout that kills the process tree). `HOME` is the home dots
//!   writes to (`--home` redirects it), `PATH` includes what this run installed. Hooks run as
//!   the user, never through sudo. Output goes to `<logs>/<pkg>.hook-<event>.log`.
//! - **Failure.** A failed hook is a run error (`hook-failed`, with the log). It fails the
//!   package's remaining hooks (skipped, reported) and, for `pre_install`, the package's
//!   install batch; a failed `pre_files` leaves that package's files alone. Everything
//!   independent carries on. Each hook is journalled (`op = hook-<event>`) and its outcome is
//!   recorded in `state.json` (`hooks_failed`).

use std::path::{Path, PathBuf};

use dots_providers::{Cmd, Ctx, Output};

/// The four hook events, in the order a package's hooks are planned.
pub const EVENTS: [&str; 4] = ["pre_install", "post_install", "pre_files", "post_files"];

/// Does this event belong to the install half of a run (as opposed to the files half)?
pub fn is_install_event(event: &str) -> bool {
    matches!(event, "pre_install" | "post_install")
}

/// One planned hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookStep {
    pub package: String,
    /// `pre_install`, `post_install`, `pre_files` or `post_files`.
    pub event: String,
    /// The command text, exactly as written in `package.toml`.
    pub command: String,
    /// The package directory: the hook's working directory.
    pub pkg_dir: PathBuf,
    /// Why it runs (`package installs`, `files change`, `failed last run`).
    pub why: String,
    /// Set when the package comes from a source the user has not reviewed: the hook always
    /// asks, even under `--yes`.
    pub always_confirm: Option<String>,
}

impl HookStep {
    /// The command's first line, for the plan, the journal and messages.
    pub fn short(&self) -> String {
        let first = self.command.lines().next().unwrap_or("").trim();
        if self.command.trim().lines().count() > 1 {
            format!("{first} ...")
        } else {
            first.to_string()
        }
    }

    /// `<pkg>:<event>`, the key of `state.json` `hooks_failed`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.package, self.event)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookStepResult {
    pub step: HookStep,
    pub ok: bool,
    /// The failure.
    pub detail: Option<String>,
    pub log: Option<PathBuf>,
    pub dur_ms: u64,
}

/// What the hooks phase is asked to build a command for.
#[derive(Debug, Clone, Copy)]
pub struct HookRequest<'a> {
    pub package: &'a str,
    pub event: &'a str,
    pub command: &'a str,
    pub pkg_dir: &'a Path,
}

/// The seam to the script runtime. The real implementation (`dots-cli`) builds the command
/// with the script provider's shell and `DOTS_*` environment; [`NoHooksPhase`] runs
/// `/bin/sh -c` in the package directory.
pub trait HooksPhase: Send + Sync {
    /// The command that runs the hook. `ctx.env` already carries what this run installed.
    fn command(&self, req: &HookRequest<'_>, ctx: &Ctx) -> Result<Cmd, String>;
}

/// Plain `/bin/sh -c <command>` in the package directory, with the process environment.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHooksPhase;

impl HooksPhase for NoHooksPhase {
    fn command(&self, req: &HookRequest<'_>, ctx: &Ctx) -> Result<Cmd, String> {
        let mut c = Cmd::new("/bin/sh")
            .args(["-c", req.command])
            .cwd(req.pkg_dir);
        c.env = ctx.env.clone();
        c.env.insert("DOTS_PKG".into(), req.package.into());
        c.env.insert("DOTS_HOOK".into(), req.event.into());
        Ok(c)
    }
}

fn log_path(ctx: &Ctx, step: &HookStep) -> PathBuf {
    ctx.log_dir.join(format!(
        "{}.hook-{}.log",
        step.package.replace(['/', '\\'], "_"),
        step.event
    ))
}

fn write_log(path: &Path, step: &HookStep, out: &Output) {
    let mut s = format!("$ {}\n", step.command.replace('\n', "; "));
    s.push_str(&out.stdout);
    if !out.stdout.is_empty() && !out.stdout.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(&out.stderr);
    if !out.stderr.is_empty() && !out.stderr.ends_with('\n') {
        s.push('\n');
    }
    if out.timed_out {
        s.push_str("[dots] killed: timeout\n");
    } else {
        s.push_str(&format!("[dots] exit status {}\n", out.status));
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")));
    let _ = std::fs::write(path, s);
}

/// Run one hook through `ctx.runner` and log it. Never called in a dry run.
pub(crate) fn run_hook(phase: &dyn HooksPhase, ctx: &Ctx, step: &HookStep) -> HookStepResult {
    let started = std::time::Instant::now();
    let done = |ok: bool, detail: Option<String>, log: Option<PathBuf>| HookStepResult {
        step: step.clone(),
        ok,
        detail,
        log,
        dur_ms: started.elapsed().as_millis() as u64,
    };
    let req = HookRequest {
        package: &step.package,
        event: &step.event,
        command: &step.command,
        pkg_dir: &step.pkg_dir,
    };
    let cmd = match phase.command(&req, ctx) {
        Ok(c) => c,
        Err(e) => return done(false, Some(e), None),
    };
    let log = log_path(ctx, step);
    match ctx.runner.run(&cmd) {
        Ok(out) => {
            write_log(&log, step, &out);
            if out.success() {
                done(true, None, Some(log))
            } else if out.timed_out {
                done(
                    false,
                    Some(format!("`{}` timed out", step.short())),
                    Some(log),
                )
            } else {
                done(
                    false,
                    Some(format!(
                        "`{}` exited with status {}",
                        step.short(),
                        out.status
                    )),
                    Some(log),
                )
            }
        }
        Err(e) => done(false, Some(e.to_string()), None),
    }
}
