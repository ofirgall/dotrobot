//! The shared command context loader: where the repo is, the machine config, facts with
//! `[facts]` overrides applied, and the platform chain (with `[platforms]` parents and
//! `platform_extra`). Every read-only command starts here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dots_core::Diagnostic;
use dots_facts::{Chain, FactOverrides, Facts, RealSources, detect};
use dots_model::{LoadErrors, MachineConfig, Repo};
use serde_json::Value;

use crate::app::Context;

/// The machine side: config and facts. Needs no repo (beyond profiles under `machines/`).
pub struct MachineCtx {
    /// The repo root in use (`--root`, else machine.toml `root`, else `~/dotfiles`).
    pub root: PathBuf,
    pub machine: MachineConfig,
    pub facts: Facts,
    pub warnings: Vec<Diagnostic>,
}

/// Machine plus repo plus the platform chain.
pub struct RepoCtx {
    pub m: MachineCtx,
    pub repo: Repo,
    pub chain: Chain,
}

fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path {
        "~" => home.to_path_buf(),
        p => match p.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(p),
        },
    }
}

fn load_machine_config(
    root: &Path,
    local: &Path,
    hostname: &str,
    warnings: &mut Vec<Diagnostic>,
) -> Result<MachineConfig, Vec<Diagnostic>> {
    match MachineConfig::load(root, local, hostname) {
        Ok(l) => {
            warnings.extend(l.warnings);
            Ok(l.value)
        }
        Err(LoadErrors { errors, .. }) => Err(errors),
    }
}

fn overrides(machine: &MachineConfig) -> FactOverrides {
    let flag = |k: &str| machine.facts.get(k).and_then(Value::as_bool);
    FactOverrides {
        sudo: flag("sudo"),
        gui: flag("gui"),
        wsl: flag("wsl"),
    }
}

/// Load the machine config and detect facts.
///
/// The root is resolved first (flag, then `DOTS_ROOT`, then the local machine.toml `root`, then
/// `~/dotfiles`), and the profile (`machines/<host>.toml`) is read against it. Facts are
/// detected once for the hostname and again only when `[facts]` overrides something.
pub fn load_machine(
    ctx: &Context,
    root_override: Option<&Path>,
) -> Result<MachineCtx, Vec<Diagnostic>> {
    let local = ctx.dirs.machine_toml(ctx.opts.machine_config.as_deref());
    let mut facts = detect(&RealSources, &FactOverrides::default());
    let mut warnings = Vec::new();

    let root = match root_override {
        Some(r) => r.to_path_buf(),
        None => match std::env::var("DOTS_ROOT").ok().filter(|v| !v.is_empty()) {
            Some(r) => expand_home(&r, &ctx.dirs.home),
            None => MachineConfig::local_root(&local)
                .map(|r| expand_home(&r, &ctx.dirs.home))
                .unwrap_or_else(|| ctx.dirs.home.join("dotfiles")),
        },
    };
    let machine = load_machine_config(&root, &local, &facts.hostname, &mut warnings)?;
    let ov = overrides(&machine);
    if ov != FactOverrides::default() {
        facts = detect(&RealSources, &ov);
    }
    Ok(MachineCtx {
        root,
        machine,
        facts,
        warnings,
    })
}

/// `[platforms.<key>] parent` declarations.
pub fn platform_parents(repo: &Repo) -> BTreeMap<String, String> {
    repo.manifest
        .platforms
        .iter()
        .filter_map(|(k, d)| Some((k.clone(), d.parent.clone()?)))
        .collect()
}

pub fn build_chain(m: &MachineCtx, repo: Option<&Repo>) -> Chain {
    let parents = repo.map(platform_parents).unwrap_or_default();
    Chain::build(&m.facts, &parents, &m.machine.platform_extra)
}

/// Load machine, repo and chain. Every problem (machine file or repo) is returned together
/// with the warnings gathered so far.
pub fn load_repo(
    ctx: &Context,
    root_override: Option<&Path>,
) -> Result<RepoCtx, (Vec<Diagnostic>, Vec<Diagnostic>)> {
    let mut m = load_machine(ctx, root_override).map_err(|e| (e, vec![]))?;
    if !m.root.join("dots.toml").is_file() {
        let root = m.root.display().to_string();
        let d = Diagnostic::from_code("not-initialized", &[("root", &root)]);
        return Err((vec![d], m.warnings));
    }
    match Repo::load(&m.root) {
        Ok(l) => {
            m.warnings.extend(l.warnings);
            let chain = build_chain(&m, Some(&l.value));
            Ok(RepoCtx {
                m,
                repo: l.value,
                chain,
            })
        }
        Err(LoadErrors { errors, warnings }) => {
            let mut w = m.warnings;
            w.extend(warnings);
            Err((errors, w))
        }
    }
}
