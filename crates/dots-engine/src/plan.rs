//! Plan = diff(resolution, installed): typed steps, batched per provider, with dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dots_core::{Diagnostic, Dirs};
use dots_model::{ProviderConfig, Repo};
use dots_providers::{Action, Ctx, Op, OpKind, Registry};
use dots_resolve::{EdgeKind, InstalledMap, Resolution, spec_for};

use crate::files::{FileStep, FilesPhase, FilesRequest};
use crate::version::{Change, Wanted, diff, parse_wanted};

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
    /// Provider context: the sandboxed home, environment and runner.
    pub ctx: &'a Ctx,
    pub dirs: &'a Dirs,
    pub scope: &'a Scope,
    /// Packages whose scripts come from a source the user has not reviewed. Their install
    /// always asks, even under `--yes`.
    pub unreviewed: &'a BTreeSet<String>,
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
    pub interactive: bool,
    /// This package is another package's provider bootstrap.
    pub bootstrap: bool,
    /// Why this step must always be confirmed, even with `--yes`.
    pub always_confirm: Option<String>,
    /// Packages (with steps) that must finish first.
    pub deps: Vec<String>,
    /// Index of the batch carrying this step.
    pub batch: usize,
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<PkgStep>,
    pub batches: Vec<Batch>,
    pub files: Vec<FileStep>,
    /// Selected active packages (those whose files are in scope).
    pub packages: Vec<String>,
    /// Packages left out because no provider could install them, with the reason.
    pub unplanned: Vec<(String, String)>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.files.is_empty()
    }

    pub fn needs_root(&self) -> bool {
        self.batches
            .iter()
            .any(|b| b.actions.iter().any(|a| a.needs_root))
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
        out
    }

    /// Does the plan do more than refresh files dots already owns?
    pub fn touches_more_than_owned_files(&self) -> bool {
        !self.steps.is_empty() || self.files.iter().any(|f| !f.owned)
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
    Ok(plan)
}

struct Draft {
    step: PkgStep,
    op: Op,
}

fn plan_packages(
    inp: &Inputs<'_>,
    selected: &[String],
    plan: &mut Plan,
) -> Result<(), Vec<Diagnostic>> {
    let selected_set: BTreeSet<&str> = selected.iter().map(String::as_str).collect();
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
        let wanted: Wanted = parse_wanted(pkg.and_then(|p| p.version.as_deref()));
        let change = diff(&wanted, installed.map(|i| i.version.as_str()));
        let (kind, op_kind, version) = match change {
            Change::None => continue,
            Change::Install(v) => (StepKind::Install, OpKind::Install, v),
            Change::Upgrade(v) => (StepKind::Upgrade, OpKind::Upgrade, v),
            Change::Downgrade(v) => (StepKind::Downgrade, OpKind::Install, Some(v)),
        };
        let needs_root_cfg = matches!(cfg, Some(ProviderConfig::Script(s)) if s.needs_root);
        let interactive = matches!(cfg, Some(ProviderConfig::Script(s)) if s.interactive);
        let is_script = choice.id == "script";
        let always_confirm = (is_script && inp.unreviewed.contains(name))
            .then(|| format!("`{name}` runs a script from a source you have not reviewed"));
        let spec = spec_for(&choice.native_name, cfg);
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
                    interactive,
                    bootstrap: bootstrap_targets.contains(name.as_str()),
                    always_confirm,
                    deps: Vec::new(),
                    batch: 0,
                },
                op,
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

    type Key = (String, Option<String>, usize, Option<String>);
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
    for (i, ((provider, instance, _, inter), pkgs)) in keyed.iter().enumerate() {
        let provider_impl = inp
            .registry
            .get_for(provider, pkgs.first().map_or("", String::as_str));
        let ops: Vec<Op> = pkgs.iter().map(|p| drafts[p].op.clone()).collect();
        let actions = match provider_impl {
            Some(p) => p.plan(&ops).map_err(|e| {
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
        d.step.batch = batch_of[name];
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

/// Relative path of `p` under `base` (for journal logs), or `p` itself.
pub(crate) fn relative(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}
