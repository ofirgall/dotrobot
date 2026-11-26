//! The real files phase: a package's `[[files]]` entries go through `dots-files`
//! (`resolve_entry` -> `plan` -> `apply`) and surface as the engine's [`FileStep`]s.
//!
//! The engine's step is a summary (package, destination, mode, action, one-line detail) plus an
//! opaque [`Payload`] holding the full `dots_files::FileStep`; `apply` takes that back out.
//! Steps that would change nothing are not planned at all, so a converged machine has an
//! empty plan.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use dots_core::{Diagnostic, Dirs};
use dots_engine::{FileStep, FileStepResult, FilesPhase, FilesRequest, Payload};
use dots_env::{Contribution, EnvCtx, Shell};
use dots_facts::{Condition, EvalCtx};
use dots_files::{
    Action, BackupKind, FilesCtx, Ledger, Mode, OutcomeStatus, expand_path, resolve_path_vars,
};
use dots_resolve::Resolution;
use serde_json::json;

use crate::context::RepoCtx;

/// What a planned step carries back to `apply`.
enum Planned {
    File(Box<dots_files::FileStep>),
    /// The `[env]` files and the single rc inject, as one step.
    Env,
}

pub struct RepoFiles<'a> {
    pub dirs: &'a Dirs,
    pub rc: &'a RepoCtx,
    pub resolution: &'a Resolution,
}

/// `~/x` for paths under home.
fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn mode_word(m: Mode) -> &'static str {
    match m {
        Mode::Link => "link",
        Mode::Copy => "copy",
        Mode::Render => "render",
        Mode::Inject => "inject",
        Mode::Dir => "dir",
        Mode::Terminfo => "terminfo",
    }
}

fn action_word(a: Action) -> &'static str {
    match a {
        Action::Unchanged => "unchanged",
        Action::Create => "create",
        Action::Update => "update",
        Action::Replace => "replace",
        Action::Remove => "remove",
        Action::Failed => "failed",
    }
}

fn message(d: &Diagnostic) -> String {
    // A required setting without a value says what to run: `msg → dots config set ...`.
    if d.code == "setting-required" {
        format!("{} \u{2192} {}", d.message, d.hint)
    } else {
        d.message.clone()
    }
}

impl RepoFiles<'_> {
    fn ctx(&self, req: &FilesRequest<'_>) -> FilesCtx {
        let mut c = self.base_ctx(req.home, req.state_dir, req.run_id);
        c.dry_run = req.dry_run;
        c
    }

    /// The files context shared by `apply`, `plan` and `status`, so they cannot disagree.
    pub fn base_ctx(&self, home: &Path, state_dir: &Path, run_id: &str) -> FilesCtx {
        let rc = self.rc;
        let mut c = FilesCtx::new(home, state_dir, &rc.m.root, "main", run_id);
        c.chain = rc.chain.levels().to_vec();
        if matches!(rc.m.facts.os.as_str(), "macos" | "linux" | "windows") {
            c.os = rc.m.facts.os.clone();
        }
        c.vars = resolve_path_vars(&rc.repo.manifest.paths, &c.chain);
        let facts: serde_json::Map<String, serde_json::Value> =
            rc.m.facts
                .entries()
                .into_iter()
                .map(|(n, v, _)| (n, v.to_json()))
                .collect();
        c.template_vars = json!({
            "os": rc.m.facts.os,
            "arch": rc.m.facts.arch,
            "hostname": rc.m.facts.hostname,
            "user": rc.m.facts.user,
            "tags": rc.m.machine.tags,
            "platform": c.chain,
            "facts": facts,
        });
        // Package settings: every applicable value with a name `<pkg>.<key>` (a file reads its
        // own package's as `setting.x`, others as `settings.<pkg>.x`), and the same in
        // `template_vars.settings` for jinja layers.
        let settings = crate::config_cmd::settings_for(rc);
        c.settings = settings
            .iter()
            .filter(|s| s.applies)
            .filter_map(|s| {
                Some(dots_files::SettingUse {
                    name: s.id(),
                    value: s.value()?.clone(),
                    scope: s.scope()?.as_str().to_string(),
                    secret: s.secret(),
                })
            })
            .collect();
        c.template_vars["settings"] = settings.vars("")["settings"].clone();
        // Conditions in `dots:if` directives and `[[files.set]] when`.
        c.cond = Some(std::sync::Arc::new(dots_files::CondEnv {
            facts: rc.m.facts.clone(),
            chain: rc.chain.clone(),
            tags: rc.m.machine.tags.iter().cloned().collect(),
            declared_tags: rc.repo.manifest.tags.keys().cloned().collect(),
        }));
        c
    }

    /// Conditions that can read package settings (`setting.x` is the lookup's own package).
    pub fn when_with<'s>(
        &'s self,
        settings: &'s dyn dots_facts::SettingLookup,
    ) -> impl Fn(&str) -> Result<bool, String> + 's {
        let tags: BTreeSet<String> = self.rc.m.machine.tags.iter().cloned().collect();
        let declared: BTreeSet<String> = self.rc.repo.manifest.tags.keys().cloned().collect();
        move |src: &str| {
            Condition::parse(src)
                .and_then(|c| {
                    c.eval(&EvalCtx {
                        facts: &self.rc.m.facts,
                        chain: &self.rc.chain,
                        tags: &tags,
                        declared_tags: &declared,
                        settings,
                    })
                })
                .map_err(|e| e.message)
        }
    }

    /// The repo-override, machine-profile and machine-override layers of one file, lowest
    /// first, for those whose path exists. The path is `overrides/` +
    /// [`dots_files::override_layer_rel`] of the resolved destination under each scope.
    pub fn extra_layers(
        &self,
        pkg: &str,
        entry: &dots_model::FileEntry,
        ctx: &FilesCtx,
    ) -> Vec<dots_files::LayerSrc> {
        let Some(dst) = dots_files::entry_dst(entry, ctx) else {
            return vec![];
        };
        let rel = dots_files::override_layer_rel(pkg, &dst, &ctx.home);
        let root = &self.rc.m.root;
        let mut scopes = vec![root.join("overrides")];
        if let Some(profile) = &self.rc.m.machine.profile {
            scopes.push(root.join("machines").join(profile).join("overrides"));
        }
        scopes.push(self.dirs.config.join("overrides"));
        scopes
            .into_iter()
            .map(|s| s.join(&rel))
            .filter(|p| p.is_file())
            .map(|path| dots_files::LayerSrc {
                label: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                path,
            })
            .collect()
    }

    /// Every `[[files]]` entry of `packages` resolved (conditions evaluated, layers found),
    /// plus the entries that could not be. `apply`, `plan` and `status` all go through this.
    pub fn resolved_files(&self, packages: &[String], ctx: &FilesCtx) -> ResolvedFiles<'_> {
        let settings = crate::config_cmd::settings_for(self.rc);
        let mut out = ResolvedFiles::default();
        // A file several packages contribute to (`mode = "fragment"`) is assembled from every
        // active package, also those outside this run, so a targeted apply cannot drop a part.
        let outside: Vec<&String> = self
            .resolution
            .order
            .iter()
            .filter(|n| {
                !packages.contains(*n) && self.resolution.get(n).is_some_and(|p| p.is_active())
            })
            .collect();
        let scoped = packages.iter().map(|n| (n, true));
        for (name, in_scope) in scoped.chain(outside.into_iter().map(|n| (n, false))) {
            let Some(pkg) = self.rc.repo.packages.get(name) else {
                continue;
            };
            let lookup = settings.lookup(name);
            let when = self.when_with(&lookup);
            // A required setting without a value blocks this package's files, and only its.
            let blocked = settings
                .missing(name)
                .first()
                .map(|s| Diagnostic::from_code("setting-required", &[("setting", &s.id())]));
            // A service's `unit` is one more owned file: linked into the manager's unit dir.
            let unit = crate::services_phase::unit_entry(self.rc, self.resolution, name);
            let entries = pkg
                .files
                .iter()
                .map(Cow::Borrowed)
                .chain(unit.map(Cow::Owned));
            for entry in entries {
                if !in_scope && entry.mode.as_deref() != Some("fragment") {
                    continue;
                }
                if let Some(d) = &blocked {
                    out.failures.push((name.clone(), entry, d.clone()));
                    continue;
                }
                let extra = self.extra_layers(name, &entry, ctx);
                match dots_files::resolve_entry(name, &pkg.dir, &entry, ctx, &when, &extra) {
                    Ok(None) => {}
                    Ok(Some(f)) => out.files.push(f),
                    Err(d) => out.failures.push((name.clone(), entry, d)),
                }
            }
        }
        if out
            .failures
            .iter()
            .any(|(_, e, _)| e.mode.as_deref() == Some("fragment"))
        {
            // One contribution is missing, so the assembled file would be wrong: leave it be.
            out.files.retain(|f| f.extras.fragment.is_none());
        } else {
            out.files = dots_files::compose_fragments(std::mem::take(&mut out.files));
        }
        out
    }

    /// Every file step of `packages`, unchanged ones included, plus the packages whose entries
    /// could not all be resolved.
    fn desired(
        &self,
        packages: &[String],
        ctx: &FilesCtx,
    ) -> (Vec<dots_files::FileStep>, BTreeSet<String>) {
        let r = self.resolved_files(packages, ctx);
        let mut steps = Vec::new();
        let mut unresolved = BTreeSet::new();
        // Keep package order: one entry at a time, as before.
        for f in &r.files {
            steps.extend(dots_files::plan(std::slice::from_ref(f), ctx));
        }
        for (name, entry, d) in r.failures {
            unresolved.insert(name.clone());
            if entry.mode.as_deref() == Some("fragment") {
                unresolved.insert(dots_files::FRAGMENT_OWNER.to_string());
            }
            steps.push(unresolved_step(&name, &entry, d, ctx));
        }
        (steps, unresolved)
    }
}

/// The outcome of [`RepoFiles::resolved_files`].
#[derive(Default)]
pub struct ResolvedFiles<'a> {
    pub files: Vec<dots_files::ResolvedFile>,
    /// Package, entry and why it could not be resolved.
    pub failures: Vec<(String, Cow<'a, dots_model::FileEntry>, Diagnostic)>,
}

/// A step standing for an entry that could not even be resolved.
fn unresolved_step(
    package: &str,
    entry: &dots_model::FileEntry,
    error: Diagnostic,
    ctx: &FilesCtx,
) -> dots_files::FileStep {
    let dst = match &entry.dst {
        Some(dots_model::PlatformString::Plain(s)) => expand_path(s, ctx).unwrap_or(s.into()),
        _ => "(unresolved)".into(),
    };
    dots_files::FileStep {
        package: package.into(),
        dst,
        mode: match entry.mode.as_deref() {
            Some("copy") => Mode::Copy,
            Some("render" | "fragment") => Mode::Render,
            Some("inject") => Mode::Inject,
            Some("dir") => Mode::Dir,
            Some("terminfo") => Mode::Terminfo,
            _ => Mode::Link,
        },
        action: Action::Failed,
        from: None,
        backup: BackupKind::None,
        link_target: None,
        gen_path: None,
        src: None,
        sha256: None,
        layers: vec![],
        marker: None,
        comment: None,
        error: Some(error),
        content: None,
    }
}

fn detail(s: &dots_files::FileStep, root: &Path) -> String {
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    let mut d = if let Some(e) = &s.error {
        message(e)
    } else if s.action == Action::Remove {
        "no longer declared".to_string()
    } else {
        match s.mode {
            Mode::Link => format!("-> {}", s.src.as_deref().map(rel).unwrap_or_default()),
            Mode::Copy => format!("from {}", s.src.as_deref().map(rel).unwrap_or_default()),
            Mode::Render => format!("{} layer(s): {}", s.layers.len(), s.layers.join(" + ")),
            Mode::Inject => format!("block `{}`", s.marker.as_deref().unwrap_or("")),
            Mode::Dir => "directory".to_string(),
            Mode::Terminfo => format!(
                "tic -x -o {}",
                s.dst
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        }
    };
    match s.backup {
        BackupKind::None => {}
        _ => d.push_str("; existing file backed up first"),
    }
    d
}

impl RepoFiles<'_> {
    /// `[env]` of every active package, in resolution order (not just this run's scope, so a
    /// targeted apply never drops the others' variables).
    fn contributions(&self) -> Vec<Contribution> {
        self.resolution
            .order
            .iter()
            .filter(|n| self.resolution.get(n).is_some_and(|p| p.is_active()))
            .filter_map(|n| {
                let pkg = self.rc.repo.packages.get(n)?;
                let e = &pkg.env;
                (!e.path.is_empty() || !e.vars.is_empty() || !e.source.is_empty())
                    .then(|| Contribution::new(n.clone(), e.clone()))
            })
            .collect()
    }

    fn shell(&self) -> Option<Shell> {
        std::env::var("SHELL")
            .ok()
            .and_then(|s| Shell::detect(&s))
            .or_else(|| Shell::detect(&self.rc.m.facts.shell))
    }

    fn env_ctx(&self, home: &Path) -> EnvCtx {
        let mut c = EnvCtx::new(home);
        c.vars = resolve_path_vars(&self.rc.repo.manifest.paths, self.rc.chain.levels());
        c
    }

    /// One step when the env files or the rc inject are not what they should be.
    fn env_step(&self, req: &FilesRequest<'_>) -> Option<FileStep> {
        let contribs = self.contributions();
        let sh = self.dirs.env_file("sh");
        let existed = sh.exists();
        if contribs.is_empty() && !existed {
            return None;
        }
        let shell = self.shell();
        let rc = shell.map(|s| dots_env::default_rc(s, req.home));
        let (stale, error) = match dots_env::generate(&contribs, &self.env_ctx(req.home)) {
            Err(e) => (true, Some(e.to_string())),
            Ok(files) => {
                let files_stale = Shell::ALL.iter().any(|s| {
                    std::fs::read_to_string(self.dirs.env_file(s.ext()))
                        .ok()
                        .as_deref()
                        != Some(files.get(*s))
                });
                let rc_stale = match (shell, &rc) {
                    (Some(s), Some(rc)) => {
                        let target = std::fs::canonicalize(rc).unwrap_or_else(|_| rc.clone());
                        let want = format!(
                            "{}\n{}\n{}\n",
                            dots_env::MARKER_OPEN,
                            dots_env::rc_line(s, &self.dirs.env_file(s.ext()), req.home),
                            dots_env::MARKER_CLOSE
                        );
                        let text = std::fs::read_to_string(target).unwrap_or_default();
                        text.matches(dots_env::MARKER_OPEN).count() != 1 || !text.contains(&want)
                    }
                    _ => false,
                };
                let rc_stale = if contribs.is_empty() {
                    // Nothing contributes: the block should be gone, not merely current.
                    rc.as_deref().is_some_and(|r| {
                        std::fs::read_to_string(std::fs::canonicalize(r).unwrap_or(r.into()))
                            .is_ok_and(|t| t.contains(dots_env::MARKER_OPEN))
                    })
                } else {
                    rc_stale
                };
                (files_stale || rc_stale, None)
            }
        };
        if !stale {
            return None;
        }
        let dst = rc.as_deref().unwrap_or(&sh);
        Some(FileStep {
            package: "env".into(),
            dst: tilde(dst, req.home),
            kind: "env".into(),
            detail: error
                .unwrap_or_else(|| "env.sh, env.fish, env.ps1 and the shell rc line".to_string()),
            owned: existed,
            action: if existed { "update" } else { "create" }.into(),
            payload: Payload::new(Planned::Env),
        })
    }

    fn sync_env(&self, req: &FilesRequest<'_>) -> Result<(), String> {
        let shell = self.shell();
        let rc = shell.map(|s| dots_env::default_rc(s, req.home));
        let contribs = self.contributions();
        if contribs.is_empty() {
            return dots_env::clear(self.dirs, &self.env_ctx(req.home), rc.as_deref())
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        dots_env::sync(
            self.dirs,
            &self.env_ctx(req.home),
            &self.contributions(),
            shell.unwrap_or(Shell::Zsh),
            rc.as_deref(),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
}

impl FilesPhase for RepoFiles<'_> {
    fn plan(&self, req: &FilesRequest<'_>) -> Result<Vec<FileStep>, String> {
        let ctx = self.ctx(req);
        let (desired, unresolved) = self.desired(req.packages, &ctx);
        let removals: Vec<_> = dots_files::plan_removals(&desired, &ctx, None)
            .into_iter()
            .filter(|r| !unresolved.contains(&r.package))
            .filter(|r| {
                // Files of a package left out of this run stay; files of packages that are
                // gone from the machine entirely, or that dropped the entry, go.
                req.packages.contains(&r.package)
                    || self
                        .resolution
                        .get(&r.package)
                        .is_none_or(|p| !p.is_active())
            })
            .collect();
        let ledger = Ledger::load(&ctx.state);
        let mut steps: Vec<FileStep> = Vec::new();
        let files_steps = desired
            .into_iter()
            .chain(removals)
            .filter(|s| s.action != Action::Unchanged)
            .map(|s| {
                let owned = s.action != Action::Replace
                    && s.action != Action::Remove
                    && ledger.get(&s.dst, s.mode, &s.package).is_some();
                FileStep {
                    package: s.package.clone(),
                    dst: tilde(&s.dst, req.home),
                    kind: mode_word(s.mode).into(),
                    detail: detail(&s, &self.rc.m.root),
                    owned,
                    action: action_word(s.action).into(),
                    payload: Payload::new(Planned::File(Box::new(s))),
                }
            })
            .collect::<Vec<_>>();
        steps.extend(files_steps);
        steps.extend(self.env_step(req));
        Ok(steps)
    }

    fn apply(
        &self,
        req: &FilesRequest<'_>,
        steps: &[FileStep],
        done: &mut dyn FnMut(FileStepResult),
    ) -> Result<(), String> {
        let ctx = self.ctx(req);
        for s in steps {
            let started = Instant::now();
            let (ok, note) = match s.payload.get::<Planned>() {
                None => (
                    false,
                    Some("internal error: file step lost its plan".into()),
                ),
                Some(Planned::Env) => match self.sync_env(req) {
                    Ok(()) => (true, None),
                    Err(e) => (false, Some(e)),
                },
                Some(Planned::File(fs)) => {
                    let out = dots_files::apply(fs, &ctx);
                    match out.status {
                        OutcomeStatus::Failed => (
                            false,
                            Some(
                                out.error
                                    .as_ref()
                                    .map_or("file action failed".into(), message),
                            ),
                        ),
                        OutcomeStatus::Skipped => (true, Some("left as is".to_string())),
                        _ => (
                            true,
                            out.backup
                                .map(|b| format!("existing file backed up to {}", b.display())),
                        ),
                    }
                }
            };
            done(FileStepResult {
                step: s.clone(),
                ok,
                detail: note,
                dur_ms: started.elapsed().as_millis() as u64,
            });
        }
        Ok(())
    }
}
