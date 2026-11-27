//! Provider trait and implementations. The trait is the stable seam (design D5); see
//! `specs/provider-contract` and ARCHITECTURE.md section 5.1.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dots_facts::Facts;

pub mod apt;
pub mod apt_repo;
pub mod brew;
pub mod cargo;
pub mod custom;
pub mod fetch;
pub mod git;
pub(crate) mod pathsrc;
mod runner;
pub mod script;
pub mod uv;
pub use fetch::{CurlFetcher, Fetcher};
pub use runner::{
    Cmd, DEFAULT_TIMEOUT, LineSink, Output, RealRunner, Runner, Stdin, with_line_sink,
};

/// Default timeout of the read-only questions asked while planning (a package's `check`,
/// `version` and `latest`): they should answer in moments, and one stuck on something nobody
/// can answer (a network that does not respond) must not hold the whole plan.
pub const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// A read-only question that ran out of time and was treated as unanswered (a `version`
/// that printed nothing in time, a `latest` that did not answer) instead of failing the plan.
/// Recorded by the provider, collected by the command that planned and shown as a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryTimeout {
    pub package: String,
    /// `version` or `latest`.
    pub what: &'static str,
    /// The limit that was hit, e.g. `60s`.
    pub timeout: String,
}

static QUERY_TIMEOUTS: Mutex<Vec<QueryTimeout>> = Mutex::new(Vec::new());

pub(crate) fn record_query_timeout(t: QueryTimeout) {
    if let Ok(mut v) = QUERY_TIMEOUTS.lock() {
        v.push(t);
    }
}

/// Take the [`QueryTimeout`]s recorded since the last call.
pub fn take_query_timeouts() -> Vec<QueryTimeout> {
    QUERY_TIMEOUTS
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

pub type Result<T> = std::result::Result<T, ProviderError>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider `{provider}` is unavailable: {reason}")]
    Unavailable { provider: String, reason: String },
    #[error("`{command}` failed with exit status {status}")]
    CommandFailed { command: String, status: i32 },
    /// A failure with a stable error code (`check-failed-after-install`, `timeout`, ...) from
    /// `docs/errors.md`, and the log holding the command output.
    #[error("{message}")]
    Coded {
        code: &'static str,
        message: String,
        log: Option<PathBuf>,
    },
    #[error("{0}")]
    Other(String),
}

/// How well a provider can install an exact version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Yes,
    Limited,
    No,
}

/// Declared capabilities. Behavior that depends on one must be visible to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub batch_install: bool,
    pub exact_version: Support,
    pub pin: bool,
    pub needs_root: bool,
    pub uninstall: bool,
    /// Never run two batches of this provider concurrently.
    pub exclusive_lock: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Ready {
        bin_path: PathBuf,
    },
    /// Usable after installing this package first.
    Bootstrap {
        pkg: String,
    },
    Unsupported,
}

/// Execution context. Construct with `Ctx::default()` and field updates to stay
/// source-compatible as it grows.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub dry_run: bool,
    pub offline: bool,
    /// All external commands go through this (default: real processes).
    pub runner: Arc<dyn Runner>,
    /// Environment providers pass to commands (and read `PATH`-like lookups from). Providers
    /// never read the process environment directly.
    pub env: BTreeMap<String, String>,
    /// The (possibly sandboxed) home directory.
    pub home: PathBuf,
    pub cache_dir: PathBuf,
    /// `logs/<run-id>/`: `exec` writes `<pkg>.<action>.log` here.
    pub log_dir: PathBuf,
    /// Downloads (apt keys, script `fetch` files) go through this (default: `curl`).
    pub fetcher: Arc<dyn Fetcher>,
    /// How long a read-only question (script `check`, `version`, `latest`) may take. Shorter
    /// than a script's install `timeout`, which it never exceeds.
    pub query_timeout: Duration,
}

impl Default for Ctx {
    fn default() -> Self {
        Ctx {
            dry_run: false,
            offline: false,
            runner: Arc::new(RealRunner),
            env: BTreeMap::new(),
            home: PathBuf::new(),
            cache_dir: PathBuf::new(),
            log_dir: PathBuf::new(),
            fetcher: Arc::new(CurlFetcher),
            query_timeout: DEFAULT_QUERY_TIMEOUT,
        }
    }
}

/// A package as the provider names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Spec {
    /// Provider-native name.
    pub name: String,
    /// Where the package comes from: a brew tap, an apt PPA, a registry or url. Not an option
    /// bag; per-package options live in [`Spec::opts`].
    pub source: Option<String>,
    /// Typed per-package provider options (`cask`, `git`, `rev`, `features`, `locked`, `kind`,
    /// `python`, `url`, `dest`, ...). Each provider documents the keys it reads and ignores the
    /// rest. Booleans are `"true"` / `"false"`.
    pub opts: BTreeMap<String, String>,
}

impl Spec {
    pub fn new(name: &str) -> Self {
        Spec {
            name: name.to_string(),
            source: None,
            opts: BTreeMap::new(),
        }
    }

    pub fn source(mut self, source: &str) -> Self {
        self.source = Some(source.to_string());
        self
    }

    /// Builder: set one option.
    pub fn opt(mut self, key: &str, value: &str) -> Self {
        self.opts.insert(key.to_string(), value.to_string());
        self
    }

    /// An option's value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.opts.get(key).map(String::as_str)
    }

    /// A boolean option (`"true"`/`"false"`), or `default` when absent.
    pub fn flag(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some("true") => true,
            Some("false") => false,
            _ => default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub pinned: bool,
    /// Revision or url the install came from.
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Install,
    Upgrade,
    Remove,
    Pin,
    Unpin,
}

/// A desired operation: `Op = Install|Upgrade|Remove|Pin|Unpin {spec, version}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub kind: OpKind,
    pub spec: Spec,
    pub version: Option<String>,
}

/// One concrete step: exactly what `exec` runs, and what `dots plan -v` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub kind: OpKind,
    /// The ops this action carries out (several when batched).
    pub ops: Vec<Op>,
    /// The exact command line.
    pub argv: Vec<String>,
    pub needs_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub success: bool,
    /// Path to the log with the command's output, when there is one.
    pub log: Option<PathBuf>,
}

/// An executable a package put on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinEntry {
    pub package: String,
    pub bin: String,
    pub path: PathBuf,
}

pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn caps(&self) -> Caps;

    /// Instances of providers that return the same key never run concurrently (a script
    /// package's `lock`, for packages that rewrite one shared file). `None`: no restriction.
    fn lock_key(&self) -> Option<String> {
        None
    }

    /// Is the backend usable here? Providers find their own binary, not via `PATH`.
    fn availability(&self, facts: &Facts) -> Availability;

    /// Like [`Provider::availability`] but with the execution context, so a sandboxed home and
    /// an explicit (possibly empty) `PATH` apply. Must not depend on `PATH`. Providers that
    /// probe the filesystem or run commands override this; the default ignores the context.
    fn availability_in(&self, _ctx: &Ctx, facts: &Facts) -> Availability {
        self.availability(facts)
    }

    /// ONE batched call returning everything installed, keyed by provider-native name.
    fn installed(&self, ctx: &Ctx) -> Result<BTreeMap<String, Installed>>;

    /// Latest available versions. Batched; cacheable by the caller.
    fn latest(&self, ctx: &Ctx, specs: &[Spec]) -> Result<BTreeMap<String, Latest>>;

    /// Pure: turn ops into the exact commands, with no side effects.
    fn plan(&self, ops: &[Op]) -> Result<Vec<Action>>;

    /// Like [`Provider::plan`], told which specs of this provider stay wanted after the run
    /// (the whole desired set, not just this batch). Providers with shared resources (apt
    /// repositories) use it to drop a resource only with its last user. The default ignores
    /// `wanted`.
    fn plan_with_wanted(&self, ops: &[Op], wanted: &[Spec]) -> Result<Vec<Action>> {
        let _ = wanted;
        self.plan(ops)
    }

    fn exec(&self, ctx: &Ctx, action: &Action) -> Result<Outcome>;

    /// `spec` is installed and satisfies its version, but something it declares is not set up
    /// on this machine yet, so it must be upgraded anyway (apt: the `repo` is not configured,
    /// so the package still comes from the distribution archive). Returns the reason; it is
    /// shown in the plan. Asked only for installed packages the version rules leave alone, and
    /// must read local state only. Default: never.
    fn stale_reason(&self, ctx: &Ctx, spec: &Spec) -> Option<String> {
        let _ = (ctx, spec);
        None
    }

    /// `action` (an install or upgrade of several packages) exited non-zero: the per-package
    /// actions to run instead, so one bad name fails only itself. `None` (the default): a batch
    /// failure fails every package in it.
    fn retry_each(&self, action: &Action) -> Option<Vec<Action>> {
        let _ = action;
        None
    }

    /// Packages other than the ones `action` names that carrying it out would also remove
    /// (reverse dependencies the package manager would take along), found by simulating it.
    /// Only meaningful for removals. The default cannot tell and reports none.
    fn removal_collateral(&self, _ctx: &Ctx, _action: &Action) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Executables each installed package put on disk. Batched.
    fn bins(&self, ctx: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>>;

    /// The installed packages the user asked for, as opposed to the ones the package manager
    /// pulled in as dependencies (`brew info --installed-on-request`, `apt-mark showmanual`).
    /// `Ok(None)`: this provider cannot tell, so every install counts as a root.
    fn roots(&self, _ctx: &Ctx) -> Result<Option<std::collections::BTreeSet<String>>> {
        Ok(None)
    }

    /// Optional reverse lookup from paths to the owning package.
    fn owner_of(&self, _ctx: &Ctx, _paths: &[PathBuf]) -> Result<BTreeMap<PathBuf, String>> {
        Ok(BTreeMap::new())
    }
}

/// The providers known to this build, by id.
///
/// Most providers are one shared instance (`brew`, `apt`, ...). Some are one instance per
/// package: `script` (each package has its own commands), `git` (repos registered per package)
/// and user-defined providers whose declaration is per repo. Those register with
/// [`Registry::register_for`], and [`Registry::get_for`] finds the package's instance and falls
/// back to the shared one.
#[derive(Default)]
pub struct Registry {
    providers: BTreeMap<String, Box<dyn Provider>>,
    /// provider id -> package name -> instance.
    scoped: BTreeMap<String, BTreeMap<String, Box<dyn Provider>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add (or replace) the shared instance under its own id.
    pub fn register(&mut self, provider: Box<dyn Provider>) -> &mut Self {
        self.providers.insert(provider.id().to_string(), provider);
        self
    }

    pub fn with(mut self, provider: Box<dyn Provider>) -> Self {
        self.register(provider);
        self
    }

    /// Add (or replace) the instance that handles package `pkg` for the provider's id.
    pub fn register_for(&mut self, pkg: &str, provider: Box<dyn Provider>) -> &mut Self {
        self.scoped
            .entry(provider.id().to_string())
            .or_default()
            .insert(pkg.to_string(), provider);
        self
    }

    pub fn with_for(mut self, pkg: &str, provider: Box<dyn Provider>) -> Self {
        self.register_for(pkg, provider);
        self
    }

    /// The shared instance of `id`.
    pub fn get(&self, id: &str) -> Option<&dyn Provider> {
        self.providers.get(id).map(|b| b.as_ref())
    }

    /// The instance to use for `pkg` under provider `id`: the package's own, else the shared one.
    pub fn get_for(&self, id: &str, pkg: &str) -> Option<&dyn Provider> {
        self.scoped
            .get(id)
            .and_then(|m| m.get(pkg))
            .map(|b| b.as_ref())
            .or_else(|| self.get(id))
    }

    /// Does `pkg` have its own instance under provider `id`?
    pub fn has_scoped(&self, id: &str, pkg: &str) -> bool {
        self.scoped.get(id).is_some_and(|m| m.contains_key(pkg))
    }

    /// Every provider id with at least one instance, sorted.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        let mut ids: Vec<&str> = self
            .providers
            .keys()
            .chain(self.scoped.keys())
            .map(String::as_str)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
    }

    /// Every instance of `id`: the shared one, then per-package ones (with their package).
    pub fn instances<'a>(
        &'a self,
        id: &str,
    ) -> impl Iterator<Item = (Option<&'a str>, &'a dyn Provider)> + 'a {
        let shared = self.providers.get(id).map(|p| (None, p.as_ref()));
        let scoped = self
            .scoped
            .get(id)
            .into_iter()
            .flat_map(|m| m.iter().map(|(k, p)| (Some(k.as_str()), p.as_ref())));
        shared.into_iter().chain(scoped)
    }
}
