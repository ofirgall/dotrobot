//! The real services phase: the `[service]` of the active packages goes through
//! `dots-services` (one state read per manager, then only the difference), and a package's
//! `unit` file is linked as an owned file by the files phase.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use dots_core::Dirs;
use dots_engine::{
    Payload, PlanError, ServiceStep, ServiceStepResult, ServicesPhase, ServicesRequest,
};
use dots_model::{FileEntry, Layer, Layers, Package, PlatformString};
use dots_providers::Ctx;
use dots_resolve::Resolution;
use dots_services::{Ledger, Manager, Row, Service, Sys, Tools};

use crate::context::RepoCtx;

pub struct RepoServices<'a> {
    pub rc: &'a RepoCtx,
    pub resolution: &'a Resolution,
    pub ctx: &'a Ctx,
}

/// The user dots runs as: the `gui/<uid>` launchd domain belongs to them, whoever owns the
/// home directory. (There is no uid on Windows, and nothing there uses it.)
#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

pub fn sys<'a>(ctx: &'a Ctx, home: &Path) -> Sys<'a> {
    Sys {
        runner: &*ctx.runner,
        tools: Tools::from_env(&ctx.env),
        uid: current_uid(),
        home: home.to_path_buf(),
    }
}

/// A service name that must not reach a command: the registered `service-name-invalid`
/// diagnostic (the message names which field is wrong).
fn name_diagnostic(services: &[Service]) -> Result<(), dots_core::Diagnostic> {
    dots_services::check_names(services).map_err(|e| {
        let mut d = dots_core::Diagnostic::from_code(
            "service-name-invalid",
            &[("name", &e.name), ("package", &e.package)],
        );
        d.message = format!("{} ({})", d.message, e.what);
        d
    })
}

/// [`name_diagnostic`] as the one line `status` shows.
fn name_error(services: &[Service]) -> Result<(), String> {
    name_diagnostic(services).map_err(|d| format!("{} {}", d.message, d.hint))
}

/// The files a service's package reads its config from: every `src` and layer of its
/// `[[files]]`, and the unit. A change to any of them (linked files included, which never
/// produce a files step) restarts the service.
fn config_sources(pkg: &Package) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for f in &pkg.files {
        out.extend(f.src.iter().map(|s| pkg.dir.join(s)));
        match &f.layers {
            Some(Layers::List(l)) => out.extend(l.iter().map(|l| {
                pkg.dir.join(match l {
                    Layer::Path(p) => p,
                    Layer::Conditional { src, .. } => src,
                })
            })),
            Some(Layers::Auto { dir, .. }) => out.push(pkg.dir.join(dir)),
            None => {}
        }
    }
    if let Some(u) = pkg.service.as_ref().and_then(|s| s.unit.as_ref()) {
        out.push(pkg.dir.join(u));
    }
    out
}

/// Fingerprint of each service's config sources, by service key.
fn fingerprints(rc: &RepoCtx, services: &[Service]) -> BTreeMap<String, String> {
    services
        .iter()
        .filter_map(|s| {
            let pkg = rc.repo.packages.get(&s.package)?;
            Some((
                s.key(),
                dots_services::config_hash(&pkg.dir, &config_sources(pkg)),
            ))
        })
        .collect()
}

/// The manager `pkg`'s `[service]` uses here, or `None` when it is inert on this OS.
fn manager_for(rc: &RepoCtx, resolution: &Resolution, name: &str) -> Option<Manager> {
    let decl = rc.repo.packages.get(name)?.service.as_ref()?;
    let provider = resolution
        .get(name)
        .and_then(|p| p.chosen_provider())
        .map(|c| c.id.as_str());
    dots_services::select_manager(&rc.m.facts.os, &decl.manager, provider)
}

/// The services of `packages` that apply on this machine.
pub fn services_for(rc: &RepoCtx, resolution: &Resolution, packages: &[String]) -> Vec<Service> {
    let mut out = Vec::new();
    for name in packages {
        let Some(pkg) = rc.repo.packages.get(name) else {
            continue;
        };
        let Some(decl) = &pkg.service else { continue };
        let Some(manager) = manager_for(rc, resolution, name) else {
            continue;
        };
        out.push(Service {
            package: name.clone(),
            name: if decl.name.is_empty() {
                name.clone()
            } else {
                decl.name.clone()
            },
            manager,
            enabled: decl.enabled,
            running: decl.running,
            unit: decl.unit.clone(),
        });
    }
    out
}

/// The `[[files]]` entry that links a service's `unit` into the manager's unit directory,
/// so the unit is an owned file like any other.
pub fn unit_entry(rc: &RepoCtx, resolution: &Resolution, name: &str) -> Option<FileEntry> {
    let decl = rc.repo.packages.get(name)?.service.as_ref()?;
    let unit = decl.unit.as_ref()?;
    let manager = manager_for(rc, resolution, name)?;
    let dir = dots_services::unit_dir(manager)?;
    let file = unit.rsplit('/').next().filter(|f| !f.is_empty())?;
    Some(FileEntry {
        dst: Some(PlatformString::Plain(format!("{dir}/{file}"))),
        mode: Some("link".into()),
        src: Some(unit.clone()),
        ..FileEntry::default()
    })
}

impl RepoServices<'_> {
    fn active_packages(&self) -> Vec<String> {
        self.resolution
            .order
            .iter()
            .filter(|n| self.resolution.get(n).is_some_and(|p| p.is_active()))
            .cloned()
            .collect()
    }
}

impl ServicesPhase for RepoServices<'_> {
    fn plan(&self, req: &ServicesRequest<'_>) -> Result<Vec<ServiceStep>, PlanError> {
        let services = services_for(self.rc, self.resolution, req.packages);
        name_diagnostic(&services)?;
        // Services dots enabled whose declaration is gone: their package is not in the
        // resolution, not active, or no longer declares the service.
        let declared: BTreeSet<String> =
            services_for(self.rc, self.resolution, &self.active_packages())
                .iter()
                .map(Service::key)
                .collect();
        let ledger = Ledger::load(req.state_dir);
        let orphans = ledger.orphans(&declared);
        let orphan_services: Vec<Service> = orphans.iter().filter_map(|o| o.service()).collect();
        name_diagnostic(&orphan_services)?;
        if services.is_empty() && orphans.is_empty() {
            return Ok(Vec::new());
        }
        let sys = sys(self.ctx, req.home);
        let all: Vec<Service> = services.iter().chain(&orphan_services).cloned().collect();
        let states = dots_services::observe(&sys, &all)?;
        // Config changed: the engine's packages (files that have a step), and the services
        // whose config sources differ from the fingerprint of their last converged run.
        let mut changed: BTreeSet<String> = req.changed.iter().cloned().collect();
        changed.extend(ledger.config_changed(&fingerprints(self.rc, &services)));
        let mut steps = dots_services::plan(&sys, &services, &states, &changed);
        steps.extend(dots_services::plan_teardown(&sys, &orphans, &states));
        Ok(steps
            .into_iter()
            .map(|s| ServiceStep {
                package: s.package.clone(),
                name: s.name.clone(),
                manager: s.manager.as_str().into(),
                action: s.action.clone(),
                detail: s.why.clone(),
                commands: s.argv.iter().map(|a| a.join(" ")).collect(),
                teardown: s.release.is_some(),
                payload: Payload::new(s),
            })
            .collect())
    }

    fn apply(
        &self,
        req: &ServicesRequest<'_>,
        steps: &[ServiceStep],
        done: &mut dyn FnMut(ServiceStepResult),
    ) -> Result<(), String> {
        let sys = sys(self.ctx, req.home);
        let mut ledger = Ledger::load(req.state_dir);
        let mut failed: BTreeSet<String> = BTreeSet::new();
        for s in steps {
            let started = Instant::now();
            let res = match s.payload.get::<dots_services::Step>() {
                None => Err("internal error: service step lost its plan".to_string()),
                Some(step) => dots_services::apply_recorded(&sys, &mut ledger, step),
            };
            if res.is_err() {
                failed.insert(format!("{}:{}", s.package, s.name));
            }
            done(ServiceStepResult {
                step: s.clone(),
                ok: res.is_ok(),
                detail: res.err(),
                dur_ms: started.elapsed().as_millis() as u64,
            });
        }
        // The teardown call runs before the files phase and carries no config: the baseline is
        // recorded by the later call.
        if req.teardown {
            return ledger.save();
        }
        // The config these services now run with is the new baseline (a failed one keeps the
        // old, so its restart is planned again).
        let services = services_for(self.rc, self.resolution, req.packages);
        for (k, h) in fingerprints(self.rc, &services) {
            if !failed.contains(&k) {
                ledger.config.insert(k, h);
            }
        }
        ledger.save()
    }
}

/// Rows for `status` (one call per manager) and the packages whose service is inert here.
pub fn status_rows(
    rc: &RepoCtx,
    resolution: &Resolution,
    dirs: &Dirs,
    ctx: &Ctx,
) -> (Vec<Row>, Vec<String>, Option<String>) {
    let packages: Vec<String> = resolution
        .order
        .iter()
        .filter(|n| resolution.get(n).is_some_and(|p| p.is_active()))
        .cloned()
        .collect();
    let services = services_for(rc, resolution, &packages);
    if let Err(e) = name_error(&services) {
        return (Vec::new(), Vec::new(), Some(e));
    }
    let inert: Vec<String> = packages
        .iter()
        .filter(|n| {
            rc.repo
                .packages
                .get(*n)
                .is_some_and(|p| p.service.is_some())
                && manager_for(rc, resolution, n).is_none()
        })
        .cloned()
        .collect();
    if services.is_empty() {
        return (Vec::new(), inert, None);
    }
    let s = sys(ctx, &dirs.home);
    match dots_services::observe(&s, &services) {
        Ok(states) => (dots_services::status(&services, &states), inert, None),
        Err(e) => (Vec::new(), inert, Some(e)),
    }
}
