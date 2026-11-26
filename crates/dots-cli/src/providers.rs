//! The provider registry for this run and the installed-state query.
//!
//! Shared providers (brew, apt, cargo, uv, git) are one instance each. `script` is one instance
//! per package that declares `[providers.script]`; `git` additionally gets one instance per
//! package declaring `[providers.git]` (its repo registered); user-defined providers come from
//! the `[providers.<id>]` tables in the repo's dots.toml.

use std::path::Path;

use dots_core::Dirs;
use dots_facts::Facts;
use dots_model::{ProviderConfig, Repo};
use dots_providers::apt::AptProvider;
use dots_providers::brew::BrewProvider;
use dots_providers::cargo::CargoProvider;
use dots_providers::custom::{CustomDef, CustomProvider};
use dots_providers::git::GitProvider;
use dots_providers::script::{ScriptConfig, ScriptEnv, ScriptProvider};
use dots_providers::uv::UvProvider;
use dots_providers::{Ctx, Registry};
use dots_resolve::InstalledMap;

/// The execution context for provider calls: a (possibly sandboxed) home, an environment
/// snapshot with `HOME` pointing at it, and the log directory for the run.
pub fn exec_ctx(dirs: &Dirs, offline: bool, dry_run: bool, run_id: &str) -> Ctx {
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    env.insert("HOME".into(), dirs.home.to_string_lossy().into_owned());
    Ctx {
        dry_run,
        offline,
        env,
        home: dirs.home.clone(),
        cache_dir: dirs.cache.clone(),
        log_dir: dirs.logs(run_id),
        ..Ctx::default()
    }
}

/// `DOTS_PROVIDERS=brew,script` (or `none`) limits which providers exist for this run; unset
/// means all. It keeps sandboxed runs and tests independent of what the host has installed.
fn enabled(ctx: &Ctx, id: &str) -> bool {
    match ctx.env.get("DOTS_PROVIDERS") {
        None => true,
        Some(list) => list.split(',').any(|p| p.trim() == id),
    }
}

/// Registry with only the shared providers (no repo needed).
pub fn shared_registry(ctx: &Ctx) -> Registry {
    let mut r = Registry::new();
    if enabled(ctx, "brew") {
        r.register(Box::new(BrewProvider::new()));
    }
    if enabled(ctx, "apt") {
        r.register(Box::new(AptProvider::new()));
    }
    if enabled(ctx, "cargo") {
        r.register(Box::new(CargoProvider::for_ctx(ctx)));
    }
    if enabled(ctx, "uv") {
        r.register(Box::new(UvProvider::for_ctx(ctx)));
    }
    if enabled(ctx, "git") {
        r.register(Box::new(GitProvider::for_ctx(ctx)));
    }
    r
}

fn script_config(s: &dots_model::Script, bins: Vec<String>) -> ScriptConfig {
    ScriptConfig {
        check: s.check.clone(),
        version: s.version.clone(),
        latest: s.latest.clone(),
        install: s.install.clone(),
        upgrade: s.upgrade.clone(),
        uninstall: s.uninstall.clone(),
        shell: s.shell.clone(),
        needs_root: s.needs_root,
        interactive: s.interactive,
        versionless: s.versionless,
        timeout: s.timeout.clone(),
        bins,
    }
}

/// Every real provider: shared ones, a script instance per scripted package, a git instance per
/// git package, and the user-defined providers of `repo`.
pub fn registry(
    dirs: &Dirs,
    ctx: &Ctx,
    facts: &Facts,
    repo: Option<&Repo>,
    root: &Path,
) -> Registry {
    let mut reg = shared_registry(ctx);
    let Some(repo) = repo else { return reg };
    for (id, table) in &repo.manifest.providers {
        if !enabled(ctx, id) {
            continue;
        }
        if let Ok(def) = CustomDef::from_json(id, table) {
            reg.register(Box::new(CustomProvider::new(def, facts)));
        }
    }
    for (name, pkg) in &repo.packages {
        for cfg in pkg.providers.values() {
            match cfg {
                ProviderConfig::Script(s) if enabled(ctx, "script") => {
                    let env = ScriptEnv {
                        pkg_dir: pkg.dir.clone(),
                        root: root.to_path_buf(),
                        state: dirs.state.clone(),
                        bin_dir: dirs.home.join(".local/bin"),
                        lib_dir: root.join("lib"),
                        tags: Vec::new(),
                        settings: Default::default(),
                        path_prefix: Vec::new(),
                    };
                    // An invalid script table is a lint matter; the package just has no
                    // script provider.
                    if let Ok(p) =
                        ScriptProvider::new(name, script_config(s, pkg.bins.clone()), env, facts)
                    {
                        reg.register_for(name, Box::new(p));
                    }
                }
                ProviderConfig::Git(g) if enabled(ctx, "git") => {
                    let native = g.name.clone().unwrap_or_else(|| name.clone());
                    if let Some(url) = &g.url {
                        let dest = dirs.data.join("git").join(&native);
                        reg.register_for(
                            name,
                            Box::new(GitProvider::for_ctx(ctx).repo(&native, url, dest)),
                        );
                    }
                }
                _ => {}
            }
        }
    }
    reg
}

/// One batched `installed()` call per provider instance; a failing provider contributes
/// nothing (the status commands report provider problems themselves).
pub fn installed(registry: &Registry, ctx: &Ctx) -> InstalledMap {
    let mut out = InstalledMap::new();
    for id in registry.ids() {
        for (_, p) in registry.instances(id) {
            if let Ok(m) = p.installed(ctx) {
                out.entry(id.to_string()).or_default().extend(m);
            }
        }
    }
    out
}
