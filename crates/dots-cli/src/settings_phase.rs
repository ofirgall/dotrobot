//! The real settings phase: `[[defaults]]`, `[[registry]]` and `[[xdg]]` of the active packages
//! go through `dots-settings` (`plan` -> `apply_step`) and surface as the engine's
//! [`SettingStep`]s, so `plan`, `apply` and `status` read the machine the same way.
//!
//! Originals are kept in `<state>/settings.json` (a sibling of the files ledger).

use std::collections::BTreeSet;
use std::time::Instant;

use dots_core::{Dirs, ExitCode, Report};
use dots_engine::{
    Payload, PlanError, RestartResult, SettingStep, SettingStepResult, SettingsPhase,
    SettingsRequest,
};
use dots_providers::Ctx;
use dots_resolve::Resolution;
use dots_settings::{Declared, Health, Ledger, Sudo, Sys, Tools};

use crate::context::RepoCtx;

pub struct RepoSettings<'a> {
    pub rc: &'a RepoCtx,
    pub resolution: &'a Resolution,
    /// The runner and environment every OS call goes through.
    pub ctx: &'a Ctx,
}

/// One row for `dots status`.
pub struct StatusRow {
    pub name: String,
    pub package: String,
    pub kind: String,
    /// `ok`, `drift`, `missing`, `failed`, `unknown` or `n/a`.
    pub state: &'static str,
    pub want: String,
    pub have: Option<String>,
    pub detail: String,
    pub adopt: Option<String>,
}

/// Is dots running as root already (then root entries need no sudo)?
#[cfg(unix)]
pub fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}

#[cfg(not(unix))]
pub fn running_as_root() -> bool {
    false
}

/// How root entries get root. `primed` is the engine's word that the run's one prompt primed
/// sudo; anything else (declined, failed, `--no-sudo`, a plan that never asked) leaves root
/// entries failing with a clear error rather than running as the user.
pub fn sudo_mode(primed: bool) -> Sudo {
    if primed {
        Sudo::Primed
    } else if running_as_root() {
        Sudo::Unneeded
    } else {
        Sudo::Unavailable
    }
}

pub fn sys<'a>(ctx: &'a Ctx, sudo: bool) -> Sys<'a> {
    Sys {
        runner: &*ctx.runner,
        tools: Tools::from_env(&ctx.env),
        sudo: sudo_mode(sudo),
    }
}

/// What `packages` declare on this machine's OS: the active settings, and the inert ones
/// (`package`, `id`, `label`).
type Inert = Vec<(String, String, String)>;

pub fn declared_for(rc: &RepoCtx, packages: &[String]) -> Result<(Vec<Declared>, Inert), String> {
    let os = rc.m.facts.os.as_str();
    let mut active = Vec::new();
    let mut inert = Vec::new();
    for name in packages {
        let Some(pkg) = rc.repo.packages.get(name) else {
            continue;
        };
        let (a, i) = dots_settings::declared(name, pkg, os)?;
        active.extend(a);
        inert.extend(i.into_iter().map(|(id, label)| (name.clone(), id, label)));
    }
    Ok((active, inert))
}

impl RepoSettings<'_> {
    fn active_packages(&self) -> Vec<String> {
        self.resolution
            .order
            .iter()
            .filter(|n| self.resolution.get(n).is_some_and(|p| p.is_active()))
            .cloned()
            .collect()
    }
}

impl SettingsPhase for RepoSettings<'_> {
    fn plan(&self, req: &SettingsRequest<'_>) -> Result<Vec<SettingStep>, PlanError> {
        let (declared, _) = declared_for(self.rc, req.packages)?;
        let ledger = Ledger::load(req.state_dir);
        // A declaration is "gone" only when no active package declares the key any more (a
        // key that moved between packages stays), and its owner is in scope, or is not in the
        // resolution or not active any more (the rule the files phase uses for removals).
        let (everything, _) = declared_for(self.rc, &self.active_packages())?;
        let ids: BTreeSet<String> = everything.iter().map(|d| d.setting.id()).collect();
        let orphans: Vec<dots_settings::Owned> = ledger
            .owned
            .values()
            .filter(|o| !ids.contains(&o.setting.id()))
            .filter(|o| {
                req.packages.contains(&o.package)
                    || self
                        .resolution
                        .get(&o.package)
                        .is_none_or(|p| !p.is_active())
            })
            .cloned()
            .collect();
        let (steps, _) = dots_settings::plan(&sys(self.ctx, req.sudo), &declared, &orphans)?;
        Ok(steps
            .into_iter()
            .map(|s| SettingStep {
                package: s.package.clone(),
                id: s.id(),
                label: s.setting.label(),
                kind: s.setting.kind().into(),
                action: s.op.as_str().into(),
                detail: s.detail(),
                root: s.root(),
                restart: s.setting.restart().to_vec(),
                requires: s.setting.requires().to_vec(),
                payload: Payload::new(s),
            })
            .collect())
    }

    fn apply(
        &self,
        req: &SettingsRequest<'_>,
        steps: &[SettingStep],
        done: &mut dyn FnMut(SettingStepResult),
    ) -> Result<(), String> {
        let sys = sys(self.ctx, req.sudo);
        let mut ledger = Ledger::load(req.state_dir);
        for s in steps {
            let started = Instant::now();
            let res = match s.payload.get::<dots_settings::Step>() {
                None => Err("internal error: setting step lost its plan".to_string()),
                Some(step) => dots_settings::apply_step_noted(&sys, &mut ledger, req.run_id, step),
            };
            done(SettingStepResult {
                step: s.clone(),
                ok: res.is_ok(),
                detail: match res {
                    Ok(note) => note,
                    Err(e) => Some(e),
                },
                dur_ms: started.elapsed().as_millis() as u64,
            });
        }
        Ok(())
    }

    fn settled(&self, _req: &SettingsRequest<'_>) -> Vec<(String, String)> {
        settled_todos(self.rc, self.resolution, self.ctx)
    }

    fn restart(&self, req: &SettingsRequest<'_>, apps: &[String]) -> Vec<RestartResult> {
        let sys = sys(self.ctx, req.sudo);
        apps.iter()
            .map(|app| {
                let r = dots_settings::restart_app(&sys, app);
                RestartResult {
                    app: app.clone(),
                    ok: r.is_ok(),
                    detail: r.err(),
                }
            })
            .collect()
    }
}

/// Rows for `status`: one batched read for all active settings, plus the inert ones.
pub fn status_rows(
    rc: &RepoCtx,
    resolution: &Resolution,
    dirs: &Dirs,
    ctx: &Ctx,
) -> Vec<StatusRow> {
    let packages: Vec<String> = resolution
        .order
        .iter()
        .filter(|n| resolution.get(n).is_some_and(|p| p.is_active()))
        .cloned()
        .collect();
    let (declared, inert) = match declared_for(rc, &packages) {
        Ok(v) => v,
        Err(e) => {
            return vec![StatusRow {
                name: "settings".into(),
                package: String::new(),
                kind: String::new(),
                state: "failed",
                want: String::new(),
                have: None,
                detail: e,
                adopt: None,
            }];
        }
    };
    let ledger = Ledger::load(&dirs.state);
    let mut rows: Vec<StatusRow> = dots_settings::status(&sys(ctx, false), &ledger, &declared)
        .into_iter()
        .map(|r| {
            let id = r.setting.id();
            let (state, detail, adopt) = match r.health {
                Health::Ok => ("ok", String::new(), None),
                Health::Drift => (
                    "drift",
                    format!(
                        "observed {}, wanted {}",
                        r.observed.clone().unwrap_or_default(),
                        r.wanted
                    ),
                    Some(format!("dots adopt {id}")),
                ),
                Health::Missing => (
                    "missing",
                    format!(
                        "is {}, wanted {}",
                        r.observed.clone().unwrap_or_default(),
                        r.wanted
                    ),
                    None,
                ),
                Health::Failed => ("failed", r.error.clone().unwrap_or_default(), None),
                Health::Unknown => (
                    "unknown",
                    "needs sudo to read; run `sudo -v` or `dots apply`".to_string(),
                    None,
                ),
            };
            StatusRow {
                name: r.setting.label(),
                package: r.package,
                kind: r.setting.kind().into(),
                state,
                want: r.wanted,
                have: r.observed,
                detail,
                adopt,
            }
        })
        .collect();
    for (package, _id, label) in inert {
        rows.push(StatusRow {
            name: label,
            package,
            kind: String::new(),
            state: "n/a",
            want: String::new(),
            have: None,
            detail: "n/a on this platform".into(),
            adopt: None,
        });
    }
    rows
}

/// `dots adopt <setting-id>`: write the observed value back into the declaring package.
/// `None` when `id` is not a setting id (so the caller handles it as a file).
pub fn adopt(ctx: &crate::app::Context, id: &str) -> Option<Report> {
    const KINDS: [&str; 4] = ["defaults:", "registry:", "xdg-mime:", "xdg-setting:"];
    if !KINDS.iter().any(|k| id.starts_with(k)) {
        return None;
    }
    let fail = |code: &str, detail: &str| {
        let d =
            dots_core::Diagnostic::from_code(code, &[("package", "settings"), ("detail", detail)]);
        Some(Report::failure("adopt", ExitCode::Error, vec![d]))
    };
    let rc = match crate::context::load_repo(ctx, None) {
        Ok(rc) => rc,
        Err((e, _)) => return Some(Report::failure("adopt", ExitCode::Error, e)),
    };
    let names: Vec<String> = rc.repo.packages.keys().cloned().collect();
    let declared = match declared_for(&rc, &names) {
        Ok((d, _)) => d,
        Err(e) => return fail("plan-failed", &e),
    };
    let Some(d) = declared.iter().find(|d| d.setting.id() == id) else {
        return fail(
            "plan-failed",
            &format!("no package declares `{id}` on this platform"),
        );
    };
    let pctx = crate::providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, "adopt");
    let ledger = Ledger::load(&ctx.dirs.state);
    let rows = dots_settings::status(&sys(&pctx, false), &ledger, std::slice::from_ref(d));
    let row = &rows[0];
    let Some(observed) = &row.observed_value else {
        return fail(
            "plan-failed",
            &format!("`{id}` is not set on this machine, nothing to adopt"),
        );
    };
    if row.health == Health::Ok {
        let mut r = Report::ok(
            "adopt",
            serde_json::json!({ "setting": id, "changed": false }),
        );
        r.headline = Some(format!("{} already matches the package", d.setting.label()));
        return Some(r);
    }
    let Some(pkg) = rc.repo.packages.get(&d.package) else {
        return fail("plan-failed", "package not found");
    };
    // The value that wins may come from an override or a platform table: edit that file.
    let at = match rc.repo.effective.as_deref() {
        Some(info) => dots_settings::locate(info, &d.package, &pkg.dir, &d.setting),
        None => dots_settings::Target {
            file: pkg.dir.join("package.toml"),
            platform: None,
        },
    };
    let toml = at.file.clone();
    if let Err(e) = dots_settings::adopt_in(&at, &d.setting, observed) {
        return fail("write-failed", &e);
    }
    let mut r = Report::ok(
        "adopt",
        serde_json::json!({
            "setting": id,
            "package": d.package,
            "changed": true,
            "value": row.observed,
        }),
    );
    r.headline = Some(format!(
        "{} is now {} in {}",
        d.setting.label(),
        row.observed.clone().unwrap_or_default(),
        toml.display()
    ));
    Some(r)
}

/// The manual actions raised for `logout` / `reboot` requirements whose check passes now:
/// `(package, reason)` pairs for the engine to close (and for `status` to hide).
pub fn settled_todos(rc: &RepoCtx, resolution: &Resolution, ctx: &Ctx) -> Vec<(String, String)> {
    let packages: Vec<String> = resolution
        .order
        .iter()
        .filter(|n| resolution.get(n).is_some_and(|p| p.is_active()))
        .cloned()
        .collect();
    let Ok((declared, _)) = declared_for(rc, &packages) else {
        return Vec::new();
    };
    dots_settings::settled(&sys(ctx, false), &declared)
        .into_iter()
        .map(|s| (s.package, s.reason))
        .collect()
}
