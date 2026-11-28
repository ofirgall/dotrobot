//! Plan = diff(resolution, installed): typed steps, batched per provider, with dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dots_core::{Diagnostic, Dirs};
use dots_model::{ProviderConfig, Repo};
use dots_providers::{Action, Ctx, Op, OpKind, Registry};
use dots_resolve::{EdgeKind, InstalledMap, Resolution, spec_for_in};

use crate::files::{FileStep, FilesPhase, FilesRequest};
use crate::hooks::{self, HookStep, HooksPhase};
use crate::locks::{LockInputs, LockStep};
use crate::phases::{
    ServiceStep, ServicesPhase, ServicesRequest, SettingStep, SettingsPhase, SettingsRequest,
};
use crate::shims::{self, ShimStep};
use dots_lock::rules::{self, Exact, Facts, Kind, Locked};

/// Which part of the resolved set to act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// Packages and bundles named on the command line; empty means everything.
    pub targets: Vec<String>,
    pub files_only: bool,
    pub packages_only: bool,
}

/// Everything planning (and applying) reads.
pub struct Inputs<'a> {
    pub repo: &'a Repo,
    pub resolution: &'a Resolution,
    pub registry: &'a Registry,
    pub installed: &'a InstalledMap,
    pub files: &'a dyn FilesPhase,
    pub settings: &'a dyn SettingsPhase,
    pub services: &'a dyn ServicesPhase,
    /// How `[hooks]` commands run.
    pub hooks: &'a dyn HooksPhase,
    /// Provider context: the sandboxed home, environment and runner.
    pub ctx: &'a Ctx,
    pub dirs: &'a Dirs,
    pub scope: &'a Scope,
    /// Packages whose scripts come from a source the user has not reviewed. Their install
    /// always asks, even under `--yes`.
    pub unreviewed: &'a BTreeSet<String>,
    /// The lock and what providers offer. `None` plans without a lock (every version spec is
    /// still honoured).
    pub locks: Option<&'a LockInputs<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Install,
    Upgrade,
    Downgrade,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Install => "install",
            StepKind::Upgrade => "upgrade",
            StepKind::Downgrade => "downgrade",
        }
    }
}

/// One package-level step: what happens to one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgStep {
    pub package: String,
    pub provider: String,
    /// The provider-native name.
    pub native: String,
    pub kind: StepKind,
    pub from: Option<String>,
    pub to: Option<String>,
    pub needs_root: bool,
    /// Why this is a sudo step (`requires_sudo`, `needs_root`, `removes the old copy as
    /// root`); `None` for an ordinary step. A plain `dots apply` skips sudo steps.
    pub sudo: Option<String>,
    pub interactive: bool,
    /// This package is another package's provider bootstrap.
    pub bootstrap: bool,
    /// Why this step must always be confirmed, even with `--yes`.
    pub always_confirm: Option<String>,
    /// Why an otherwise unneeded upgrade is planned (the provider's `stale_reason`).
    pub note: Option<String>,
    /// Packages (with steps) that must finish first.
    pub deps: Vec<String>,
    /// Index of the batch carrying this step.
    pub batch: usize,
    /// A different provider installed this package before: this step is a provider switch.
    pub switch: Option<Switch>,
}

/// The old side of a provider switch (`~ fzf apt -> script`): install with the new provider
/// first, then remove the copy dots installed with the old one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    pub provider: String,
    /// The old provider-native name.
    pub native: String,
    /// The version the old provider has installed (`None`: it is not installed any more).
    pub version: Option<String>,
    /// The removal, run after the new install succeeded. Empty when the copy stays.
    pub remove: Vec<Action>,
    /// Why the old copy stays (dots did not install it, another package wants it, the provider
    /// cannot uninstall it).
    pub keep: Option<String>,
    /// Platforms of the old provider's lock rows, dropped once the new install succeeded.
    pub lock_rows: Vec<String>,
}

impl Switch {
    /// Is there an old copy on this machine (removed or kept)?
    pub fn has_copy(&self) -> bool {
        self.version.is_some()
    }
}

/// One provider call: all actions the provider planned for a set of independent packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub id: usize,
    pub provider: String,
    /// Set when the provider instance is per-package (`script`, `git`).
    pub instance: Option<String>,
    pub packages: Vec<String>,
    pub actions: Vec<Action>,
    pub deps: Vec<usize>,
    pub interactive: bool,
    /// Its packages are sudo steps (a batch is never a mix: sudo is part of its key).
    pub sudo: bool,
}

/// One sudo step of a plan, as the warning lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SudoItem {
    /// The package, or the setting's label.
    pub name: String,
    /// `package` or `setting`.
    pub what: &'static str,
    /// The provider of a package, the kind (`defaults`) of a setting.
    pub kind: String,
    /// `+` install, `~` upgrade, change or setting, `-` removal.
    pub marker: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<PkgStep>,
    pub batches: Vec<Batch>,
    pub files: Vec<FileStep>,
    /// System setting changes (after files).
    pub settings: Vec<SettingStep>,
    /// Service actions (after settings, before hooks).
    pub services: Vec<ServiceStep>,
    /// Provider shims to create, repair or remove (after the installs, before files).
    pub shims: Vec<ShimStep>,
    /// Package hooks that will run, in the order they fire per package (last in the pipeline,
    /// after services; `pre_*` ones just before the action they precede).
    pub hooks: Vec<HookStep>,
    /// Selected active packages (those whose files are in scope).
    pub packages: Vec<String>,
    /// Packages left out because no provider could install them, with the reason.
    pub unplanned: Vec<(String, String)>,
    /// Lock rows that change or are recorded, and drift notes. Bookkeeping: it does not make
    /// an otherwise empty plan non-empty ([`Plan::is_empty`]); a lock-only update is still
    /// listed and applied ([`Plan::has_lock_changes`]).
    pub lock: Vec<LockStep>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
            && self.files.is_empty()
            && self.settings.is_empty()
            && self.services.is_empty()
            && self.shims.is_empty()
            && self.hooks.is_empty()
    }

    /// Does `apply` rewrite a `dots.lock` row that is listed in the Lock section? A plan with
    /// only this still has something to say (and `apply` something to do), though
    /// [`Plan::is_empty`] counts only the machine's changes.
    pub fn has_lock_changes(&self) -> bool {
        self.lock.iter().any(|l| l.shown() && l.rewrite)
    }

    /// Does anything need root: a provider action, or a `root = true` setting? One sudo prompt
    /// covers all of it.
    pub fn needs_root(&self) -> bool {
        self.batches
            .iter()
            .any(|b| b.actions.iter().any(|a| a.needs_root))
            || self
                .steps
                .iter()
                .filter_map(|s| s.switch.as_ref())
                .any(|w| w.remove.iter().any(|a| a.needs_root))
            || self.settings.iter().any(|s| s.root)
    }

    /// Every sudo step of the plan: packages in plan order, then root settings. (No service
    /// runs as root; a service has no sudo marker.)
    pub fn sudo_items(&self) -> Vec<SudoItem> {
        let mut out: Vec<SudoItem> = self
            .steps
            .iter()
            .filter_map(|s| {
                Some(SudoItem {
                    name: s.package.clone(),
                    what: "package",
                    kind: s.provider.clone(),
                    marker: if s.kind == StepKind::Install {
                        "+"
                    } else {
                        "~"
                    },
                    reason: s.sudo.clone()?,
                })
            })
            .collect();
        out.extend(self.settings.iter().filter(|s| s.root).map(|s| SudoItem {
            name: s.label.clone(),
            what: "setting",
            kind: s.kind.clone(),
            marker: if s.action == "delete" { "-" } else { "~" },
            reason: "root".into(),
        }));
        out
    }

    pub fn sudo_count(&self) -> usize {
        self.steps.iter().filter(|s| s.sudo.is_some()).count()
            + self.settings.iter().filter(|s| s.root).count()
    }

    pub fn step(&self, package: &str) -> Option<&PkgStep> {
        self.steps.iter().find(|s| s.package == package)
    }

    /// Reasons this plan must be confirmed whatever `--yes` says.
    pub fn always_confirm(&self, allow_downgrade: bool) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.steps {
            if s.kind == StepKind::Downgrade && !allow_downgrade {
                out.push(format!(
                    "`{}` would be downgraded {} -> {} (pass --allow-downgrade)",
                    s.package,
                    s.from.as_deref().unwrap_or("?"),
                    s.to.as_deref().unwrap_or("?")
                ));
            }
            if let Some(r) = &s.always_confirm {
                out.push(r.clone());
            }
        }
        for h in &self.hooks {
            if let Some(r) = &h.always_confirm {
                out.push(r.clone());
            }
        }
        out
    }

    /// Does the plan do more than refresh files dots already owns?
    pub fn touches_more_than_owned_files(&self) -> bool {
        !self.steps.is_empty()
            || !self.settings.is_empty()
            || !self.services.is_empty()
            || !self.shims.is_empty()
            // A hook is code: it is always shown and confirmed.
            || !self.hooks.is_empty()
            || self.files.iter().any(|f| !f.owned)
    }
}

fn expand_bundle(repo: &Repo, name: &str, out: &mut BTreeSet<String>, seen: &mut BTreeSet<String>) {
    if !seen.insert(name.to_string()) {
        return;
    }
    if let Some(b) = repo.bundles.get(name) {
        for inc in &b.include {
            expand_bundle(repo, inc, out, seen);
        }
        for m in &b.packages {
            out.insert(m.name().to_string());
        }
    }
}

/// The active packages in scope, in resolution order: everything, or the named packages and
/// bundles with their `requires` (and bootstrap) closure.
pub fn select(
    repo: &Repo,
    resolution: &Resolution,
    scope: &Scope,
) -> Result<Vec<String>, Vec<Diagnostic>> {
    if scope.targets.is_empty() {
        return Ok(resolution.order.clone());
    }
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    let mut errors = Vec::new();
    for t in &scope.targets {
        let t = t
            .strip_prefix("bundle:")
            .or(t.strip_prefix("pkg:"))
            .unwrap_or(t);
        if repo.packages.contains_key(t) {
            wanted.insert(t.to_string());
        } else if repo.bundles.contains_key(t) {
            expand_bundle(repo, t, &mut wanted, &mut BTreeSet::new());
        } else {
            errors.push(Diagnostic::from_code(
                "unknown-package",
                &[("name", t), ("suggestion", ""), ("file", "")],
            ));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    // Closure over requires and bootstrap edges (`after` only orders, it does not pull in).
    let mut stack: Vec<String> = wanted.iter().cloned().collect();
    while let Some(p) = stack.pop() {
        for e in &resolution.edges {
            if e.package == p && e.kind != EdgeKind::After && wanted.insert(e.dependency.clone()) {
                stack.push(e.dependency.clone());
            }
        }
    }
    Ok(resolution
        .order
        .iter()
        .filter(|n| wanted.contains(*n))
        .cloned()
        .collect())
}

fn nearest_step_deps(
    package: &str,
    edges: &BTreeMap<&str, Vec<&str>>,
    has_step: &BTreeSet<String>,
) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = edges.get(package).cloned().unwrap_or_default();
    while let Some(d) = stack.pop() {
        if !seen.insert(d) {
            continue;
        }
        if has_step.contains(d) {
            out.insert(d.to_string());
        } else if let Some(more) = edges.get(d) {
            stack.extend(more.iter().copied());
        }
    }
    out.into_iter().collect()
}

pub fn plan(inp: &Inputs<'_>, run_id: &str) -> Result<Plan, Vec<Diagnostic>> {
    let selected = select(inp.repo, inp.resolution, inp.scope)?;
    let mut plan = Plan {
        packages: selected
            .iter()
            .filter(|n| inp.resolution.get(n).is_some_and(|p| p.is_active()))
            .cloned()
            .collect(),
        ..Plan::default()
    };

    if !inp.scope.files_only {
        plan_packages(inp, &selected, &mut plan)?;
        plan.shims = plan_shims(inp, &plan);
    }
    if !inp.scope.packages_only {
        let req = FilesRequest {
            run_id,
            packages: &plan.packages,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: true,
        };
        plan.files = inp.files.plan(&req).map_err(|e| {
            vec![Diagnostic::from_code(
                "plan-failed",
                &[("package", "files"), ("detail", &e)],
            )]
        })?;
    }
    if !inp.scope.packages_only && !inp.scope.files_only {
        let sreq = SettingsRequest {
            run_id,
            packages: &plan.packages,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: true,
            full: inp.scope.targets.is_empty(),
            sudo: false,
        };
        plan.settings = inp
            .settings
            .plan(&sreq)
            .map_err(|e| vec![e.into_diagnostic("settings")])?;
        // A service restarts when its package's config (or unit) changes in this run.
        let changed: Vec<String> = plan
            .files
            .iter()
            .map(|f| f.package.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let vreq = ServicesRequest {
            run_id,
            packages: &plan.packages,
            home: &inp.dirs.home,
            state_dir: &inp.dirs.state,
            dry_run: true,
            changed: &changed,
            teardown: false,
        };
        plan.services = inp
            .services
            .plan(&vreq)
            .map_err(|e| vec![e.into_diagnostic("services")])?;
    }
    plan.hooks = plan_hooks(inp, &plan);
    Ok(plan)
}

/// Shim steps: see the `shims` module.
fn plan_shims(inp: &Inputs<'_>, plan: &Plan) -> Vec<ShimStep> {
    let (state, _) = crate::state::State::load(&inp.dirs.state_json());
    let installing: BTreeSet<String> = plan.steps.iter().map(|s| s.package.clone()).collect();
    let installed: BTreeSet<String> = plan
        .packages
        .iter()
        .filter(|n| {
            inp.resolution
                .get(n)
                .and_then(|p| p.chosen_provider())
                .is_some_and(|c| {
                    inp.installed
                        .get(&c.id)
                        .is_some_and(|m| m.contains_key(&c.native_name))
                })
        })
        .cloned()
        .collect();
    let path_var = inp
        .ctx
        .env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    shims::plan(&shims::ShimInputs {
        repo: inp.repo,
        resolution: inp.resolution,
        packages: &plan.packages,
        installing: &installing,
        installed: &installed,
        ledger: &state.shims,
        full: inp.scope.targets.is_empty(),
        home: &inp.dirs.home,
        path_var: &path_var,
    })
}

/// The hooks that fire in this run (see the `hooks` module): per package in plan order, those
/// whose trigger changed, plus any that failed last time or never ran (adopted package).
fn plan_hooks(inp: &Inputs<'_>, plan: &Plan) -> Vec<HookStep> {
    let (state, _) = crate::state::State::load(&inp.dirs.state_json());
    let mut out = Vec::new();
    for name in &plan.packages {
        let Some(pkg) = inp.repo.packages.get(name) else {
            continue;
        };
        let h = &pkg.hooks;
        let installs = plan.steps.iter().any(|s| s.package == *name);
        let files = plan.files.iter().any(|f| f.package == *name);
        for event in hooks::EVENTS {
            let Some(command) = (match event {
                "pre_install" => &h.pre_install,
                "post_install" => &h.post_install,
                "pre_files" => &h.pre_files,
                _ => &h.post_files,
            }) else {
                continue;
            };
            let in_scope = if hooks::is_install_event(event) {
                !inp.scope.files_only
            } else {
                !inp.scope.packages_only
            };
            let changed = if hooks::is_install_event(event) {
                installs
            } else {
                files
            };
            let key = format!("{name}:{event}");
            let failed = state.hooks_failed.contains(&key);
            // `post_install` of a package dots only adopted has never run: recorded in the
            // state once the adoption is journalled, and seen here already when this very run
            // adopts it.
            let adopting =
                event == "post_install" && plan.lock.iter().any(|l| l.package == *name && l.adopt);
            let pending = state.hooks_pending.contains(&key) || adopting;
            if !in_scope || !(changed || failed || pending) {
                continue;
            }
            out.push(HookStep {
                package: name.clone(),
                event: event.to_string(),
                command: command.clone(),
                pkg_dir: pkg.dir.clone(),
                why: if changed {
                    if hooks::is_install_event(event) {
                        "package installs".into()
                    } else {
                        "files change".into()
                    }
                } else if failed {
                    "failed last run".into()
                } else {
                    "adopted, never ran".into()
                },
                always_confirm: inp.unreviewed.contains(name).then(|| {
                    format!("`{name}` runs a `{event}` hook from a source you have not reviewed")
                }),
            });
        }
    }
    out
}

/// A package built from a git or local source takes its version from the source.
fn is_source_bound(spec: &dots_providers::Spec) -> bool {
    spec.opts.contains_key("git") || spec.opts.contains_key("path")
}

/// Does a row's rev (the install source, `https://...?rev=<full>#<short>`, or a bare sha) name
/// the pinned `want`?
fn row_names_rev(row: Option<&str>, want: &str) -> bool {
    let Some(row) = row else { return false };
    if row.contains(want) {
        return true;
    }
    let sha = row.rsplit_once('#').map_or(row, |(_, s)| s);
    sha.len() >= 7 && (want.starts_with(sha) || sha.starts_with(want))
}

/// What `apply` does about one package's version: the lock rules ([`rules::decide`]) plus the
/// two corrections that depend on the provider and the source. `status` asks this same
/// question, so it can never report a drift that `apply` would not act on.
///
/// - A package built from a git or local source cannot be restored to a lock row by
///   reinstalling (the install builds whatever the source holds now, and the plan could only
///   say "-> ?"), so moving "down" to the row is never offered. It used to raise a spurious,
///   confirmed downgrade to an unknown version after every upgrade. A pinned `rev` is the way
///   to hold a git source, and `stale_reason` still catches a real source mismatch.
/// - When no step follows (that, or a provider that cannot install an older version) and what
///   is installed is acceptable but not what the row says, the row is stale: it is rewritten
///   to what is installed, so one `apply` clears the difference instead of leaving a drift that
///   nothing resolves. An install outside dots is adopted and the row is rewritten the same way
///   (it used to stay, which left status showing a drift that `apply` never cleared).
/// - A version the provider cannot report ([`rules::UNKNOWN_VERSION`]) is never recorded and
///   never a drift on its own.
pub fn decide_package(f: &Facts<'_>, spec: &dots_providers::Spec) -> rules::Decision {
    // A git source pinned to a rev the row does not name: the spec moved past the row (or the
    // row recorded some other install), so it pins nothing and is rewritten to what ends up
    // installed. Without this an older row reads as a downgrade target.
    let unpinned;
    let f = match (spec.opts.get("rev"), f.locked.as_ref()) {
        (Some(want), Some(l)) if spec.opts.contains_key("git") && !row_names_rev(l.rev, want) => {
            unpinned = Facts {
                spec: f.spec,
                locked: None,
                installed: f.installed,
                installed_rev: f.installed_rev,
                owned: f.owned,
                offered: f.offered,
                exact: f.exact,
            };
            &unpinned
        }
        _ => f,
    };
    let mut d = rules::decide(f);
    if is_source_bound(spec)
        && !spec.opts.contains_key("rev")
        && d.step.as_ref().is_some_and(|s| s.kind == Kind::Downgrade)
    {
        d.step = None;
    }
    let Some(inst) = f.installed else { return d };
    if d.step.is_none() && rules::version_unknown(inst) {
        // The provider cannot say what is installed (a script with no version command): there
        // is nothing to record, and the row it holds is no stale claim to correct.
        d.rewrite = false;
        d.expected = None;
        return d;
    }
    if d.step.is_none()
        && !d.rewrite
        && d.drift.is_none()
        && let Some(l) = f.locked.as_ref()
        && rules::lock_satisfies(f.spec, l)
        && !rules::matches_lock(l, inst, f.installed_rev)
    {
        d.rewrite = true;
        d.expected = Some(inst.to_string());
    }
    d
}

/// Why a step for this package needs root: `requires_sudo` (the package or its provider
/// entry), or the provider (or its script) runs as root. `None` for an ordinary package.
pub fn sudo_reason(
    registry: &Registry,
    pkg: Option<&dots_model::Package>,
    cfg: Option<&ProviderConfig>,
    provider: &str,
    name: &str,
) -> Option<String> {
    let needs_root_cfg = matches!(cfg, Some(ProviderConfig::Script(s)) if s.needs_root);
    let caps_root = registry
        .get_for(provider, name)
        .is_some_and(|p| p.caps().needs_root);
    if pkg.is_some_and(|p| p.requires_sudo) || cfg.is_some_and(|c| c.common().requires_sudo) {
        Some("requires_sudo".to_string())
    } else if needs_root_cfg || caps_root {
        Some("needs_root".to_string())
    } else {
        None
    }
}

struct Draft {
    step: PkgStep,
    op: Op,
    /// May be a sudo step: part of the batch key, so a batch is all sudo or none.
    sudo_hint: bool,
}

fn plan_packages(
    inp: &Inputs<'_>,
    selected: &[String],
    plan: &mut Plan,
) -> Result<(), Vec<Diagnostic>> {
    let selected_set: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
    let (owned, _) = crate::state::State::load(&inp.dirs.state_json());
    let bootstrap_targets: BTreeSet<&str> = inp
        .resolution
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Bootstrap)
        .map(|e| e.dependency.as_str())
        .collect();

    // 1. the diff: one draft per package that has something to do.
    let mut drafts: BTreeMap<String, Draft> = BTreeMap::new();
    for name in selected {
        let Some(pr) = inp.resolution.get(name).filter(|p| p.is_active()) else {
            continue;
        };
        let Some(choice) = pr.chosen_provider() else {
            // A package that only has files needs no provider.
            let files_only = pr.provider.as_ref().is_some_and(|sel| {
                sel.candidates.is_empty() && sel.ranking == dots_resolve::Ranking::Unordered
            });
            if files_only {
                continue;
            }
            plan.unplanned
                .push((name.clone(), "no provider can install it".into()));
            continue;
        };
        let pkg = inp.repo.packages.get(name);
        let cfg = pkg.and_then(|p| p.providers.get(&choice.id));
        let installed = inp
            .installed
            .get(&choice.id)
            .and_then(|m| m.get(&choice.native_name));
        let spec = spec_for_in(&choice.native_name, cfg, pkg.map(|p| p.dir.as_path()));
        let exact = inp
            .registry
            .get_for(&choice.id, name)
            .map_or(Exact::Limited, |p| match p.caps().exact_version {
                dots_providers::Support::Yes => Exact::Yes,
                dots_providers::Support::Limited => Exact::Limited,
                dots_providers::Support::No => Exact::No,
            });
        let platform = dots_lock::platform_for(&choice.id, inp.locks.map_or("", |l| l.machine_key));
        // A row another provider wrote (the package switched providers) pins nothing here.
        let row = inp
            .locks
            .and_then(|l| l.lock.pkg(name, &platform))
            .filter(|r| r.provider == choice.id);
        let locked: Option<Locked<'_>> = row.map(Locked::from);
        let installed_v = installed.map(|i| i.version.as_str());
        let offered = inp.locks.and_then(|l| {
            let wanted_range = pr.wanted.is_range();
            ((wanted_range || (locked.is_some() && exact != Exact::Yes))
                && !l.upgrade.contains_key(name))
            .then(|| {
                l.offers
                    .offered(&choice.id, name, &choice.native_name, &spec)
            })
            .flatten()
        });
        let mut decision = match inp.locks.and_then(|l| l.upgrade.get(name)) {
            Some(t) => rules::Decision {
                step: Some(rules::Step {
                    kind: if installed_v.is_some() {
                        Kind::Upgrade
                    } else {
                        Kind::Install
                    },
                    version: t.version.clone(),
                }),
                rewrite: true,
                expected: t.to.clone(),
                ..Default::default()
            },
            None => decide_package(
                &Facts {
                    spec: &pr.wanted,
                    locked,
                    installed: installed_v,
                    installed_rev: installed.and_then(|i| i.source.as_deref()),
                    owned: owned
                        .packages
                        .get(name)
                        .is_some_and(|o| o.provider == choice.id),
                    offered: offered.as_deref(),
                    exact,
                },
                &spec,
            ),
        };
        let source_bound = is_source_bound(&spec);
        // Installed and fine by the version rules, but the provider says something it declares
        // is missing on this machine (an apt repository): upgrade from there.
        let mut note = None;
        let decided = match decision.step.take() {
            Some(step) => Some(step),
            None if installed.is_some() => inp
                .registry
                .get_for(&choice.id, name)
                .and_then(|p| p.stale_reason(inp.ctx, &spec))
                .map(|reason| {
                    note = Some(reason);
                    rules::Step {
                        kind: Kind::Upgrade,
                        version: None,
                    }
                }),
            None => None,
        };
        // A stale-reason step moves the install, so the row must follow what ends up installed
        // (a rebuilt git crate otherwise keeps the old rev in its row).
        if note.is_some() {
            decision.rewrite = true;
        }
        if decision.rewrite || decision.adopt || decision.drift.is_some() {
            plan.lock.push(LockStep {
                package: name.clone(),
                provider: choice.id.clone(),
                native: choice.native_name.clone(),
                platform: platform.clone(),
                from: row.map(|r| r.shown()),
                to: decision.expected.clone(),
                rewrite: decision.rewrite,
                adopt: decision.adopt,
                drift: decision.drift.clone(),
                has_step: decided.is_some(),
                installed: installed.map(|i| i.version.clone()),
                installed_source: installed.and_then(|i| i.source.clone()),
                spec: Some(spec.clone()),
                git_ref: match &pr.wanted {
                    dots_model::vspec::Spec::Git(r) => Some(r.clone()),
                    _ => None,
                },
            });
        }
        let Some(step) = decided else { continue };
        // (A source-bound package takes its version from the source.)
        let (kind, op_kind, version) = match step.kind {
            Kind::Install => (StepKind::Install, OpKind::Install, step.version),
            Kind::Upgrade => (StepKind::Upgrade, OpKind::Upgrade, step.version),
            Kind::Downgrade => (StepKind::Downgrade, OpKind::Install, step.version),
        };
        let version = if source_bound { None } else { version };
        let needs_root_cfg = matches!(cfg, Some(ProviderConfig::Script(s)) if s.needs_root);
        let interactive = matches!(cfg, Some(ProviderConfig::Script(s)) if s.interactive);
        let is_script = choice.id == "script";
        let always_confirm = (is_script && inp.unreviewed.contains(name))
            .then(|| format!("`{name}` runs a script from a source you have not reviewed"));
        let switch = plan_switch(inp, &owned, name, &choice.id);
        let uv_system = matches!(cfg, Some(ProviderConfig::Uv(u)) if u.break_system_packages);
        let sudo = sudo_reason(inp.registry, pkg, cfg, &choice.id, name).or_else(|| {
            switch
                .as_ref()
                .is_some_and(|w| w.remove.iter().any(|a| a.needs_root))
                .then(|| "removes the old copy as root".to_string())
        });
        // Known only once the provider planned its actions (uv checks the target directory);
        // until then it keeps such a package apart from the ordinary ones.
        let sudo_hint = sudo.is_some() || uv_system;
        let op = Op {
            kind: op_kind,
            spec,
            version: version.clone(),
        };
        drafts.insert(
            name.clone(),
            Draft {
                step: PkgStep {
                    package: name.clone(),
                    provider: choice.id.clone(),
                    native: choice.native_name.clone(),
                    kind,
                    from: installed.map(|i| i.version.clone()),
                    to: version,
                    needs_root: needs_root_cfg,
                    sudo,
                    interactive,
                    bootstrap: bootstrap_targets.contains(name.as_str()),
                    always_confirm,
                    note,
                    deps: Vec::new(),
                    batch: 0,
                    switch,
                },
                op,
                sudo_hint,
            },
        );
    }

    // 2. dependencies between steps, through packages that need no step.
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &inp.resolution.edges {
        if selected_set.contains(e.package.as_str()) && selected_set.contains(e.dependency.as_str())
        {
            edges.entry(&e.package).or_default().push(&e.dependency);
        }
    }
    let has_step: BTreeSet<String> = drafts.keys().cloned().collect();
    for (name, d) in drafts.iter_mut() {
        d.step.deps = nearest_step_deps(name, &edges, &has_step);
    }

    // 3. levels, then batches per (provider, instance, level).
    let order: Vec<&String> = selected
        .iter()
        .filter(|n| drafts.contains_key(*n))
        .collect();
    let mut level: BTreeMap<String, usize> = BTreeMap::new();
    // `order` is topological; repeat to a fixpoint in case an `after` edge points backwards.
    for _ in 0..=order.len() {
        let mut changed = false;
        for n in &order {
            let l = drafts[*n]
                .step
                .deps
                .iter()
                .map(|d| level.get(d).copied().unwrap_or(0) + 1)
                .max()
                .unwrap_or(0);
            if level.get(*n).copied() != Some(l) {
                level.insert((*n).clone(), l);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    type Key = (String, Option<String>, usize, Option<String>, bool);
    let mut keyed: Vec<(Key, Vec<String>)> = Vec::new();
    for n in &order {
        let d = &drafts[*n];
        let scoped = inp.registry.has_scoped(&d.step.provider, n);
        let key: Key = (
            d.step.provider.clone(),
            scoped.then(|| (*n).clone()),
            level[*n],
            // an interactive step is alone in its batch
            d.step.interactive.then(|| (*n).clone()),
            d.sudo_hint,
        );
        match keyed.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push((*n).clone()),
            None => keyed.push((key, vec![(*n).clone()])),
        }
    }
    // Plain batches first (by level), interactive ones last.
    keyed.sort_by_key(|(k, _)| (k.3.is_some(), k.2));

    let mut batch_of: BTreeMap<String, usize> = BTreeMap::new();
    for (i, (_, pkgs)) in keyed.iter().enumerate() {
        for p in pkgs {
            batch_of.insert(p.clone(), i);
        }
    }
    for (i, ((provider, instance, _, inter, _), pkgs)) in keyed.iter().enumerate() {
        let provider_impl = inp
            .registry
            .get_for(provider, pkgs.first().map_or("", String::as_str));
        let ops: Vec<Op> = pkgs.iter().map(|p| drafts[p].op.clone()).collect();
        let actions = match provider_impl {
            // The whole desired set of this provider, not just this batch: a provider with a
            // shared resource (an apt repository) drops it only with its last user.
            Some(p) => p
                .plan_with_wanted(&ops, &wanted_specs(inp, provider))
                .map_err(|e| {
                    vec![Diagnostic::from_code(
                        "plan-failed",
                        &[("package", &pkgs.join(", ")), ("detail", &e.to_string())],
                    )]
                })?,
            None => {
                return Err(vec![Diagnostic::from_code(
                    "provider-unavailable",
                    &[("provider", provider), ("package", &pkgs[0])],
                )]);
            }
        };
        let mut deps: BTreeSet<usize> = BTreeSet::new();
        for p in pkgs {
            for d in &drafts[p].step.deps {
                let b = batch_of[d];
                if b != i {
                    deps.insert(b);
                }
            }
        }
        plan.batches.push(Batch {
            id: i,
            provider: provider.clone(),
            instance: instance.clone(),
            packages: pkgs.clone(),
            actions,
            deps: deps.into_iter().collect(),
            interactive: inter.is_some(),
            sudo: false,
        });
    }

    // 4. root: an action says so, or the package's own config does.
    for (name, d) in drafts.iter_mut() {
        let b = &plan.batches[batch_of[name]];
        let by_action = b
            .actions
            .iter()
            .any(|a| a.needs_root && a.ops.iter().any(|o| o.spec.name == d.step.native));
        d.step.needs_root = d.step.needs_root || by_action;
        if by_action && d.step.sudo.is_none() {
            d.step.sudo = Some("needs_root".into());
        }
        d.step.batch = batch_of[name];
    }
    for b in &mut plan.batches {
        b.sudo = b.packages.iter().any(|p| drafts[p].step.sudo.is_some());
    }
    // Actions of root-needing script packages are marked too.
    for b in &mut plan.batches {
        let root_pkg = b
            .packages
            .iter()
            .any(|p| drafts[p].step.needs_root && b.provider == "script");
        if root_pkg {
            for a in &mut b.actions {
                a.needs_root = true;
            }
        }
    }

    plan.steps = order.iter().map(|n| drafts[*n].step.clone()).collect();
    // Bootstraps lead, otherwise keep resolution order (stable).
    plan.steps.sort_by_key(|s| !s.bootstrap);
    Ok(())
}

/// Is this package's install with `new` a switch away from another provider? The old provider
/// comes from dots' own records: `state.json` (dots installed it) first, then lock rows of
/// another provider. Only a copy `state.json` says dots installed is removed; one that only the
/// lock knows (adopted, pre-existing) stays and is reported.
fn plan_switch(
    inp: &Inputs<'_>,
    owned: &crate::state::State,
    name: &str,
    new: &str,
) -> Option<Switch> {
    let rows: Vec<&dots_lock::PkgRow> = inp
        .locks
        .map(|l| {
            l.lock
                .pkgs
                .iter()
                .filter(|r| r.name == name && r.provider != new)
                .collect()
        })
        .unwrap_or_default();
    let by_state = owned.packages.get(name).filter(|o| o.provider != new);
    let (provider, native, dots_installed) = match (by_state, rows.first()) {
        (Some(o), _) => (o.provider.clone(), o.name.clone(), true),
        (None, Some(r)) => {
            let native = inp
                .repo
                .packages
                .get(name)
                .and_then(|p| p.providers.get(&r.provider))
                .and_then(|c| c.common().name)
                .unwrap_or_else(|| name.to_string());
            (r.provider.clone(), native, false)
        }
        (None, None) => return None,
    };
    let lock_rows: Vec<String> = rows
        .iter()
        .filter(|r| r.provider == provider)
        .map(|r| r.platform.clone())
        .collect();
    let version = inp
        .installed
        .get(&provider)
        .and_then(|m| m.get(&native))
        .map(|i| i.version.clone());
    let mut sw = Switch {
        provider: provider.clone(),
        native: native.clone(),
        version,
        remove: Vec::new(),
        keep: None,
        lock_rows,
    };
    if !sw.has_copy() {
        return Some(sw);
    }
    if !dots_installed {
        sw.keep = Some("it was already installed when dots took it over, so dots leaves it".into());
        return Some(sw);
    }
    // Another active package of the same provider wants the same native package.
    let wanted_elsewhere = inp.resolution.active().any(|r| {
        r.name != name
            && r.chosen_provider()
                .is_some_and(|c| c.id == provider && c.native_name == native)
    });
    if wanted_elsewhere {
        sw.keep = Some("another package still uses it".into());
        return Some(sw);
    }
    let pkg = inp.repo.packages.get(name);
    let spec = spec_for_in(
        &native,
        pkg.and_then(|p| p.providers.get(&provider)),
        pkg.map(|p| p.dir.as_path()),
    );
    let planned = inp.registry.get_for(&provider, name).map(|p| {
        p.plan_with_wanted(
            &[Op {
                kind: OpKind::Remove,
                spec,
                version: None,
            }],
            &wanted_specs(inp, &provider),
        )
        .map_err(|e| e.to_string())
    });
    match planned {
        Some(Ok(actions)) => sw.remove = actions,
        Some(Err(e)) => sw.keep = Some(e),
        None => sw.keep = Some(format!("`{provider}` is not available to remove it")),
    }
    Some(sw)
}

/// Every spec of `provider`'s packages that stays desired after this run: all active packages
/// of the resolution that chose it, whether or not they are part of this run's batches.
fn wanted_specs(inp: &Inputs<'_>, provider: &str) -> Vec<dots_providers::Spec> {
    inp.resolution
        .order
        .iter()
        .filter_map(|name| {
            let pr = inp.resolution.get(name).filter(|p| p.is_active())?;
            let choice = pr.chosen_provider().filter(|c| c.id == provider)?;
            let pkg = inp.repo.packages.get(name);
            let cfg = pkg.and_then(|p| p.providers.get(&choice.id));
            Some(spec_for_in(
                &choice.native_name,
                cfg,
                pkg.map(|p| p.dir.as_path()),
            ))
        })
        .collect()
}

/// Relative path of `p` under `base` (for journal logs), or `p` itself.
pub(crate) fn relative(base: &Path, p: &Path) -> String {
    // Relative paths land in the journal and state files, which must read the same on every
    // OS: `/`-separated.
    match p.strip_prefix(base) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}
