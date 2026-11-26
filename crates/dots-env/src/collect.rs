use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use dots_model::EnvSpec;

/// One active package's `[env]` table.
#[derive(Debug, Clone)]
pub struct Contribution {
    pub package: String,
    pub spec: EnvSpec,
}

impl Contribution {
    pub fn new(package: impl Into<String>, spec: EnvSpec) -> Self {
        Contribution {
            package: package.into(),
            spec,
        }
    }
}

/// What path expansion needs: `~` and `{home}`-style built-ins plus resolved `[paths]` vars.
#[derive(Debug, Clone)]
pub struct EnvCtx {
    pub home: PathBuf,
    pub vars: BTreeMap<String, String>,
}

impl EnvCtx {
    pub fn new(home: &Path) -> Self {
        EnvCtx {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
        }
    }

    fn files_ctx(&self) -> dots_files::FilesCtx {
        let mut c = dots_files::FilesCtx::new(&self.home, &self.home, &self.home, "", "");
        c.vars = self.vars.clone();
        c
    }

    /// Expand `~`, `{var}` and `%VAR%`; an unknown variable leaves the text as written.
    pub(crate) fn expand(&self, s: &str) -> String {
        dots_files::expand_path(s, &self.files_ctx())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| s.to_string())
    }
}

/// The merged environment of a set of contributions, built incrementally.
///
/// This is the in-run propagation hook: the engine keeps one `EnvOverlay`, calls
/// [`add_package`](Self::add_package) after each package installs, and passes
/// [`runner_env`](Self::runner_env) to every later command
/// (`dots_providers::runner::Command::envs`).
#[derive(Debug, Clone)]
pub struct EnvOverlay {
    ctx: EnvCtx,
    /// Per-package expanded PATH entries, in add order.
    groups: Vec<Vec<String>>,
    vars: BTreeMap<String, String>,
    sources: Vec<String>,
}

impl EnvOverlay {
    pub fn new(ctx: EnvCtx) -> Self {
        EnvOverlay {
            ctx,
            groups: Vec::new(),
            vars: BTreeMap::new(),
            sources: Vec::new(),
        }
    }

    pub fn from_contributions(ctx: EnvCtx, contribs: &[Contribution]) -> Self {
        let mut o = Self::new(ctx);
        for c in contribs {
            o.add_package(&c.spec);
        }
        o
    }

    pub fn ctx(&self) -> &EnvCtx {
        &self.ctx
    }

    /// Fold one package's contribution in. It takes precedence over everything added before.
    pub fn add_package(&mut self, spec: &EnvSpec) {
        self.groups
            .push(spec.path.iter().map(|p| self.ctx.expand(p)).collect());
        for (k, v) in &spec.vars {
            self.vars.insert(k.clone(), self.ctx.expand(v));
        }
        for s in &spec.source {
            let s = self.ctx.expand(s);
            if !self.sources.contains(&s) {
                self.sources.push(s);
            }
        }
    }

    /// Final PATH entries, highest precedence first, deduplicated.
    pub fn path_entries(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in self.groups.iter().rev().flatten() {
            if !out.contains(e) {
                out.push(e.clone());
            }
        }
        out
    }

    /// Variables, expanded.
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// The variables to set on a child process: every contributed variable, plus `PATH` as the
    /// overlay entries prepended to `base_path` (normally the process's `PATH`). `PATH` is
    /// omitted when nothing was contributed, so the child just inherits.
    pub fn runner_env(&self, base_path: Option<&OsStr>) -> BTreeMap<String, String> {
        let mut env = self.vars.clone();
        env.remove("PATH");
        let mut parts: Vec<PathBuf> = self.path_entries().into_iter().map(PathBuf::from).collect();
        if !parts.is_empty() {
            if let Some(base) = base_path {
                parts.extend(std::env::split_paths(base));
            }
            let mut seen = Vec::new();
            parts.retain(|p| {
                let fresh = !seen.contains(p);
                if fresh {
                    seen.push(p.clone());
                }
                fresh
            });
            if let Ok(joined) = std::env::join_paths(parts) {
                env.insert("PATH".into(), joined.to_string_lossy().into_owned());
            }
        }
        env
    }
}
