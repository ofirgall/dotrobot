//! What every read-only inspection command (`status`, `outdated`, `bins`, `which`, `doctor`)
//! starts from: the loaded repo, the provider registry, the batched `installed()` answers
//! (queried in parallel) and the resolution. Also the latest-version cache and the small
//! helpers they share.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use dots_core::{Diagnostic, Dirs, ExitCode, Report};
use dots_engine::parse_wanted;
use dots_env::{Contribution, EnvCtx, EnvOverlay};
use dots_providers::{Ctx, Installed, Registry, Spec};
use dots_resolve::{InstalledMap, ProviderChoice, Resolution, spec_for_in};
use serde_json::{Value, json};

use crate::app::Context;
use crate::context::{RepoCtx, load_repo};
use crate::providers;

/// Upper bound on concurrent provider and subprocess calls.
const WORKERS: usize = 16;

/// `latest()` answers are reused for this long (ARCHITECTURE 7.3).
pub const LATEST_TTL_SECS: u64 = 6 * 60 * 60;

/// Run `f` over `items` on a small thread pool; results come back in input order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..WORKERS.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    out.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

/// Branch, commit and cleanliness of the repo root (all `None` outside a git checkout).
#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub dirty: bool,
}

fn git_out(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn git_info(root: &Path) -> GitInfo {
    let Some(commit) = git_out(root, &["rev-parse", "--short=7", "HEAD"]) else {
        return GitInfo::default();
    };
    GitInfo {
        branch: git_out(root, &["symbolic-ref", "--short", "-q", "HEAD"]),
        commit: Some(commit),
        dirty: git_out(root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()),
    }
}

/// Everything loaded for one inspection run.
pub struct Loaded {
    pub rc: RepoCtx,
    pub registry: Registry,
    pub pctx: Ctx,
    pub installed: InstalledMap,
    /// Provider instances whose `installed()` call failed, with the reason. A shared provider
    /// is keyed by its id, a per-package instance (`script`, `git`) by `id/package`; read it
    /// with [`Loaded::failure`].
    pub failed: BTreeMap<String, String>,
    pub resolution: Resolution,
    pub warnings: Vec<Diagnostic>,
    pub git: GitInfo,
}

/// Key of `Loaded::failed`: the provider id, plus the package for a per-package instance.
fn failure_key(id: &str, pkg: Option<&str>) -> String {
    match pkg {
        Some(p) => format!("{id}/{p}"),
        None => id.to_string(),
    }
}

/// One `installed()` call per provider instance, run on a thread pool. A failing provider
/// contributes nothing and is reported in the second value.
pub fn installed_parallel(
    registry: &Registry,
    ctx: &Ctx,
) -> (InstalledMap, BTreeMap<String, String>) {
    let jobs: Vec<(&str, Option<&str>, &dyn dots_providers::Provider)> = registry
        .ids()
        .flat_map(|id| registry.instances(id).map(move |(pkg, p)| (id, pkg, p)))
        .collect();
    let results = par_map(&jobs, |(_, _, p)| p.installed(ctx));
    let mut map = InstalledMap::new();
    let mut failed = BTreeMap::new();
    for ((id, pkg, _), r) in jobs.iter().zip(results) {
        match r {
            Ok(m) => map.entry((*id).to_string()).or_default().extend(m),
            Err(e) => {
                failed.entry(failure_key(id, *pkg)).or_insert(e.to_string());
            }
        }
    }
    (map, failed)
}

/// Resolution diagnostics split like `plan` does: a package nothing can install is a warning
/// (its row says so), anything else is an error.
fn split(res: &Resolution, mut warnings: Vec<Diagnostic>) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
    let mut errors = Vec::new();
    for d in &res.errors {
        if d.code == "no-provider-satisfies" {
            warnings.push(d.clone());
        } else {
            errors.push(d.clone());
        }
    }
    warnings.extend(res.warnings.iter().cloned());
    (errors, warnings)
}

pub fn failure(
    cmd: &str,
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    exit: ExitCode,
) -> Report {
    let mut r = Report::failure(cmd, exit, errors);
    r.warnings = warnings;
    r
}

/// Load the repo, query every provider, resolve.
pub fn load(ctx: &Context, cmd: &str) -> Result<Loaded, Box<Report>> {
    let rc =
        load_repo(ctx, None).map_err(|(e, w)| Box::new(failure(cmd, e, w, ExitCode::Error)))?;
    let pctx = providers::with_env_path(
        providers::exec_ctx(&ctx.dirs, ctx.opts.offline, false, cmd),
        &rc,
        &ctx.dirs,
    );
    let settings = crate::config_cmd::settings_for(&rc);
    let registry = providers::registry(
        &ctx.dirs,
        &pctx,
        &rc.m.facts,
        Some(&rc.repo),
        &rc.m.root,
        providers::ScriptInputs {
            tags: &rc.m.machine.tags,
            settings: &settings,
        },
    );
    assemble(rc, registry, pctx).map_err(|(e, w)| Box::new(failure(cmd, e, w, ExitCode::Error)))
}

/// Everything after loading: the provider queries (in parallel with the git lookup) and the
/// resolution. Resolution errors are returned with the warnings gathered so far.
pub fn assemble(
    rc: RepoCtx,
    registry: Registry,
    pctx: Ctx,
) -> Result<Loaded, (Vec<Diagnostic>, Vec<Diagnostic>)> {
    let ((installed, failed), git) = std::thread::scope(|s| {
        let git = s.spawn(|| git_info(&rc.m.root));
        let inst = installed_parallel(&registry, &pctx);
        (inst, git.join().unwrap_or_default())
    });
    // Never touches the network: what providers offer is whatever `outdated` cached.
    let resolution = dots_resolve::resolve_full(
        &dots_resolve::Inputs {
            repo: &rc.repo,
            machine: &rc.m.machine,
            facts: &rc.m.facts,
            chain: &rc.chain,
            providers: &registry,
            installed: &installed,
        },
        &providers::CachedOffers::new(&pctx.cache_dir),
        &crate::config_cmd::settings_for(&rc),
    );
    let (errors, mut warnings) = split(&resolution, rc.m.warnings.clone());
    warnings.extend(providers::take_query_warnings());
    if !errors.is_empty() {
        return Err((errors, warnings));
    }
    Ok(Loaded {
        rc,
        registry,
        pctx,
        installed,
        failed,
        resolution,
        warnings,
        git,
    })
}

/// An active package with its chosen provider and what that provider says is installed.
pub struct ActivePkg<'a> {
    pub name: &'a str,
    pub choice: Option<&'a ProviderChoice>,
    pub installed: Option<&'a Installed>,
}

impl Loaded {
    /// Why the query of provider `id` failed for package `pkg`: its own instance, else the
    /// shared one. One package's failing script never marks another package's.
    pub fn failure(&self, id: &str, pkg: &str) -> Option<&String> {
        if self.registry.has_scoped(id, pkg) {
            self.failed.get(&failure_key(id, Some(pkg)))
        } else {
            self.failed.get(id)
        }
    }

    pub fn active(&self) -> impl Iterator<Item = ActivePkg<'_>> {
        self.resolution.active().map(|res| {
            let choice = res.chosen_provider();
            let installed = choice.and_then(|c| {
                self.installed
                    .get(&c.id)
                    .and_then(|m| m.get(&c.native_name))
            });
            ActivePkg {
                name: &res.name,
                choice,
                installed,
            }
        })
    }

    /// `[env]` contributions of every active package, dependencies first.
    pub fn contributions(&self) -> Vec<Contribution> {
        self.resolution
            .active()
            .filter_map(|p| {
                let e = &self.rc.repo.packages.get(&p.name)?.env;
                (!e.path.is_empty() || !e.vars.is_empty() || !e.source.is_empty())
                    .then(|| Contribution::new(p.name.clone(), e.clone()))
            })
            .collect()
    }

    /// The directories dots' generated env puts on PATH, highest precedence first.
    pub fn env_path_entries(&self, dirs: &Dirs) -> Vec<String> {
        let mut c = EnvCtx::new(&dirs.home);
        c.vars =
            dots_files::resolve_path_vars(&self.rc.repo.manifest.paths, self.rc.chain.levels());
        EnvOverlay::from_contributions(c, &self.contributions()).path_entries()
    }

    /// The PATH the user's shell has: the generated env's entries (a sourced env file puts them
    /// first) followed by the live `PATH`.
    pub fn path_dirs(&self, dirs: &Dirs) -> Vec<PathBuf> {
        let live = self.pctx.env.get("PATH").cloned().unwrap_or_default();
        let mut out: Vec<PathBuf> = Vec::new();
        let env = self.env_path_entries(dirs);
        let live: Vec<&str> = live.split(':').filter(|s| !s.is_empty()).collect();
        for d in env.iter().map(String::as_str).chain(live) {
            let p = PathBuf::from(d);
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }

    /// Specs to ask `latest()` about: every installed active package, with its provider.
    pub fn latest_requests(&self) -> Vec<LatestRequest> {
        self.active()
            .filter_map(|a| {
                let c = a.choice?;
                a.installed?;
                let pkg = self.rc.repo.packages.get(a.name);
                let cfg = pkg.and_then(|p| p.providers.get(&c.id));
                Some(LatestRequest {
                    package: a.name.to_string(),
                    provider: c.id.clone(),
                    spec: spec_for_in(&c.native_name, cfg, pkg.map(|p| p.dir.as_path())),
                })
            })
            .collect()
    }
}

pub struct LatestRequest {
    pub package: String,
    pub provider: String,
    pub spec: Spec,
}

/// `~/x` for paths under home.
pub fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Is `latest` something dots would move `have` to? A newer release only counts when it still
/// satisfies the wanted version (an exact pin is never outdated), and a pinned install never is.
pub fn is_outdated(wanted: Option<&str>, have: &str, latest: &str, pinned: bool) -> bool {
    dots_engine::is_outdated(&parse_wanted(wanted), have, latest, pinned)
}

// ------------------------------------------------------------------ latest-version cache

/// One provider's cached `latest()` answers: `<cache>/latest/<provider>.json`.
#[derive(Debug, Clone, Default)]
pub struct CacheFile {
    /// Seconds since the epoch of the last refresh.
    pub fetched: u64,
    /// Provider-native name to its latest version; `None` means "asked, no answer".
    pub versions: BTreeMap<String, Option<String>>,
}

impl CacheFile {
    pub fn fresh(&self, now: u64) -> bool {
        now.saturating_sub(self.fetched) < LATEST_TTL_SECS
    }

    fn to_json(&self) -> Value {
        json!({ "schema": 1, "fetched": self.fetched, "versions": self.versions })
    }

    fn from_json(v: &Value) -> Option<CacheFile> {
        let fetched = v.get("fetched")?.as_u64()?;
        let versions = v
            .get("versions")?
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().map(str::to_string)))
            .collect();
        Some(CacheFile { fetched, versions })
    }
}

pub struct LatestCache {
    dir: PathBuf,
}

impl LatestCache {
    pub fn at(dir: PathBuf) -> Self {
        LatestCache { dir }
    }

    pub fn new(dirs: &Dirs) -> Self {
        LatestCache {
            dir: dirs.latest_cache(),
        }
    }

    fn path(&self, provider: &str) -> PathBuf {
        self.dir.join(format!("{provider}.json"))
    }

    /// A missing or unreadable file is an empty cache.
    pub fn load(&self, provider: &str) -> Option<CacheFile> {
        let text = std::fs::read_to_string(self.path(provider)).ok()?;
        CacheFile::from_json(&serde_json::from_str(&text).ok()?)
    }

    pub fn store(&self, provider: &str, file: &CacheFile) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut text =
            serde_json::to_string_pretty(&file.to_json()).map_err(std::io::Error::other)?;
        text.push('\n');
        dots_files::atomic_write(&self.path(provider), text.as_bytes())
    }

    /// Cached latest versions of every provider, whatever their age.
    pub fn load_all(&self) -> BTreeMap<String, CacheFile> {
        let mut out = BTreeMap::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return out;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json")
                && let Some(f) = self.load(id)
            {
                out.insert(id.to_string(), f);
            }
        }
        out
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The cached latest version of `native` under `provider`.
pub fn cached_latest<'a>(
    cache: &'a BTreeMap<String, CacheFile>,
    provider: &str,
    native: &str,
) -> Option<&'a str> {
    cache.get(provider)?.versions.get(native)?.as_deref()
}

/// Fixtures for in-process tests: a repo in a temp dir, a registry of fake providers and a
/// `Loaded` assembled from them, with no process spawning beyond `git`.
#[cfg(test)]
pub mod testing {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use dots_facts::{Chain, Facts};
    use dots_model::{MachineConfig, Repo};
    use dots_providers::{
        Action, Availability, BinEntry, Caps, Ctx, Installed, Latest, Op, Outcome, Provider,
        Registry, Result, Spec,
    };
    use dots_testkit::FakeProvider;

    use super::*;
    use crate::context::MachineCtx;

    pub struct Fixture {
        pub tmp: tempfile::TempDir,
        pub root: PathBuf,
        pub dirs: Dirs,
    }

    pub fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    impl Fixture {
        /// `pkgs`: (name, package.toml body after the header). All are in bundle `all`.
        pub fn new(pkgs: &[(&str, &str)]) -> Fixture {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("repo");
            write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
            let mut names = Vec::new();
            for (name, body) in pkgs {
                names.push(format!("\"{name}\""));
                write(
                    &root.join(format!("packages/{name}/package.toml")),
                    &format!("schema = 1\nname = \"{name}\"\n{body}"),
                );
            }
            write(
                &root.join("bundles/all.toml"),
                &format!("schema = 1\npackages = [{}]\n", names.join(", ")),
            );
            let home = tmp.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            let dirs = Dirs::resolve(Some(&home), &dots_core::ProcessEnv);
            Fixture { tmp, root, dirs }
        }

        pub fn facts(sudo: bool) -> Facts {
            Facts {
                os: "linux".into(),
                arch: "x86_64".into(),
                distro: Some("debian".into()),
                has_sudo: sudo,
                hostname: "box".into(),
                user: "u".into(),
                ..Default::default()
            }
        }

        /// Load with `registry`, the given facts and a PATH of `path` (colon separated).
        pub fn load(&self, registry: Registry, facts: Facts, path: &str) -> Loaded {
            let repo = Repo::load(&self.root).unwrap().value;
            let chain = Chain::build(&facts, &BTreeMap::new(), &[]);
            let machine = MachineConfig {
                bundles: vec!["all".into()],
                ..Default::default()
            };
            let rc = RepoCtx {
                m: MachineCtx {
                    root: self.root.clone(),
                    machine,
                    facts,
                    warnings: vec![],
                },
                repo,
                chain,
            };
            let mut pctx = providers::exec_ctx(&self.dirs, false, false, "test");
            pctx.env.insert("PATH".into(), path.to_string());
            match assemble(rc, registry, pctx) {
                Ok(l) => l,
                Err((e, _)) => panic!("{e:?}"),
            }
        }
    }

    /// A fake provider whose `bins()` reports real files.
    pub struct BinsProvider {
        pub inner: FakeProvider,
        /// (package, path of its executable)
        pub bins: Vec<(String, PathBuf)>,
    }

    impl Provider for BinsProvider {
        fn id(&self) -> &'static str {
            self.inner.id()
        }
        fn caps(&self) -> Caps {
            self.inner.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.inner.availability(f)
        }
        fn installed(&self, c: &Ctx) -> Result<BTreeMap<String, Installed>> {
            self.inner.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> Result<BTreeMap<String, Latest>> {
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> Result<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> Result<Outcome> {
            self.inner.exec(c, a)
        }
        fn bins(&self, _: &Ctx, installed: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
            Ok(self
                .bins
                .iter()
                .filter(|(p, _)| installed.contains_key(p))
                .map(|(p, path)| BinEntry {
                    package: p.clone(),
                    bin: path.file_name().unwrap().to_string_lossy().into_owned(),
                    path: path.clone(),
                })
                .collect())
        }
    }

    /// An executable file.
    pub fn exe(path: &Path) {
        write(path, "#!/bin/sh\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}
