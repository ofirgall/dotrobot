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

/// The execution context for provider calls: the log directory for the run, the cache dir and
/// the process environment.
///
/// `--home <dir>` (spec reconcile-engine, "Sandboxed home") redirects the user paths dots
/// WRITES: file destinations, config, state, cache and data dirs. It does not redirect the
/// provider tools: `brew`, `apt`, `cargo` and `uv` keep the real `HOME` (in `env` and as
/// `Ctx::home`, where they look for `~/.cargo`, `~/.local/bin/uv` and the like). Pointing them
/// at an empty scratch home made `brew info --installed` miss tapped formulae and made
/// cargo/uv look for themselves in a directory that has nothing. Only when the process has no
/// `HOME` does the (possibly sandboxed) `dirs.home` stand in.
pub fn exec_ctx(dirs: &Dirs, offline: bool, dry_run: bool, run_id: &str) -> Ctx {
    exec_ctx_from(dirs, std::env::vars().collect(), offline, dry_run, run_id)
}

fn exec_ctx_from(
    dirs: &Dirs,
    mut env: std::collections::BTreeMap<String, String>,
    offline: bool,
    dry_run: bool,
    run_id: &str,
) -> Ctx {
    let home = match env.get("HOME").filter(|h| !h.is_empty()) {
        Some(h) => std::path::PathBuf::from(h),
        None => {
            env.insert("HOME".into(), dirs.home.to_string_lossy().into_owned());
            dirs.home.clone()
        }
    };
    // Seconds, or a duration like `90s`, `5m`; anything else keeps the default.
    let query_timeout = env
        .get("DOTS_QUERY_TIMEOUT")
        .and_then(|v| dots_providers::script::parse_duration(v).ok())
        .filter(|d| !d.is_zero())
        .unwrap_or(dots_providers::DEFAULT_QUERY_TIMEOUT);
    Ctx {
        dry_run,
        offline,
        query_timeout,
        env,
        home,
        cache_dir: dirs.cache.clone(),
        log_dir: dirs.logs(run_id),
        ..Ctx::default()
    }
}

/// Put the `[env] path` directories of every active package in front of the inherited `PATH`
/// in `pctx`, so provider commands (script `check`/`version`/`install`, hooks, a declarative
/// provider's `available` and `installed`) find tools installed into `~/.local/bin`,
/// `~/.bun/bin` and the like, as the user's shell does. Which packages are active is a pure
/// computation (no provider is asked), so this runs before the real registry exists.
pub fn with_env_path(mut pctx: Ctx, rc: &crate::context::RepoCtx, dirs: &Dirs) -> Ctx {
    let settings = crate::config_cmd::settings_for(rc);
    let none = Registry::new();
    let installed = InstalledMap::new();
    let res = dots_resolve::resolve_full(
        &dots_resolve::Inputs {
            repo: &rc.repo,
            machine: &rc.m.machine,
            facts: &rc.m.facts,
            chain: &rc.chain,
            providers: &none,
            installed: &installed,
        },
        &dots_resolve::NoOffers,
        &settings,
    );
    let contribs: Vec<dots_env::Contribution> = res
        .active()
        .filter_map(|p| {
            let e = &rc.repo.packages.get(&p.name)?.env;
            (!e.path.is_empty()).then(|| dots_env::Contribution::new(p.name.clone(), e.clone()))
        })
        .collect();
    if contribs.is_empty() {
        return pctx;
    }
    let mut c = dots_env::EnvCtx::new(&dirs.home);
    c.vars = dots_files::resolve_path_vars(&rc.repo.manifest.paths, rc.chain.levels());
    let overlay = dots_env::EnvOverlay::from_contributions(c, &contribs);
    let base = pctx
        .env
        .get("PATH")
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    if let Some(path) = overlay.runner_env(base.as_deref()).remove("PATH") {
        pctx.env.insert("PATH".into(), path);
    }
    pctx
}

/// `DOTS_PROVIDERS=brew,script` (or `none`) limits which providers exist for this run; unset
/// means all. It keeps sandboxed runs and tests independent of what the host has installed.
fn enabled(ctx: &Ctx, id: &str) -> bool {
    match ctx.env.get("DOTS_PROVIDERS") {
        None => true,
        Some(list) => list.split(',').any(|p| p.trim() == id),
    }
}

/// What a script provider reads from the machine: its package's settings become
/// `DOTS_SETTING_<KEY>` and the machine tags `DOTS_TAGS`. Secrets are included (a script may
/// need its token) and, like every setting value here, are never written to logs or reports.
#[derive(Clone, Copy)]
pub struct ScriptInputs<'a> {
    pub tags: &'a [String],
    pub settings: &'a dots_config::Settings,
}

/// A setting value as an environment variable: lists newline-joined, bools `1`/`0`.
fn env_value(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(env_value).collect::<Vec<_>>().join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The applicable settings of `package` that have a value, by key.
pub(crate) fn script_settings(
    settings: &dots_config::Settings,
    package: &str,
) -> std::collections::BTreeMap<String, String> {
    settings
        .of_package(package)
        .filter(|s| s.applies)
        .filter_map(|s| Some((s.key.clone(), env_value(s.value()?))))
        .collect()
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
        lock: s.lock.clone(),
        bins,
        fetch: s.fetch.as_ref().and_then(|f| {
            Some(dots_providers::script::FetchSpec {
                url: f.url.clone()?,
                sha256: f.sha256.clone(),
            })
        }),
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
    inputs: ScriptInputs<'_>,
) -> Registry {
    registry_with_handles(dirs, ctx, facts, repo, root, inputs).0
}

/// The script provider instances of a run, by package, so the caller can read what they
/// downloaded (`ScriptProvider::fetched_sha256`) once they have run.
#[derive(Default)]
pub struct ScriptHandles(pub std::collections::BTreeMap<String, std::sync::Arc<ScriptProvider>>);

/// A script provider shared with [`ScriptHandles`]: the registry owns a box, the caller keeps
/// the `Arc`.
struct Shared(std::sync::Arc<ScriptProvider>);

impl dots_providers::Provider for Shared {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn caps(&self) -> dots_providers::Caps {
        self.0.caps()
    }
    fn lock_key(&self) -> Option<String> {
        self.0.lock_key()
    }
    fn availability(&self, facts: &Facts) -> dots_providers::Availability {
        self.0.availability(facts)
    }
    fn availability_in(&self, ctx: &Ctx, facts: &Facts) -> dots_providers::Availability {
        self.0.availability_in(ctx, facts)
    }
    fn installed(
        &self,
        ctx: &Ctx,
    ) -> dots_providers::Result<std::collections::BTreeMap<String, dots_providers::Installed>> {
        self.0.installed(ctx)
    }
    fn latest(
        &self,
        ctx: &Ctx,
        specs: &[dots_providers::Spec],
    ) -> dots_providers::Result<std::collections::BTreeMap<String, dots_providers::Latest>> {
        self.0.latest(ctx, specs)
    }
    fn plan(
        &self,
        ops: &[dots_providers::Op],
    ) -> dots_providers::Result<Vec<dots_providers::Action>> {
        self.0.plan(ops)
    }
    fn exec(
        &self,
        ctx: &Ctx,
        action: &dots_providers::Action,
    ) -> dots_providers::Result<dots_providers::Outcome> {
        self.0.exec(ctx, action)
    }
    fn bins(
        &self,
        ctx: &Ctx,
        installed: &std::collections::BTreeMap<String, dots_providers::Installed>,
    ) -> dots_providers::Result<Vec<dots_providers::BinEntry>> {
        self.0.bins(ctx, installed)
    }
    fn owner_of(
        &self,
        ctx: &Ctx,
        paths: &[std::path::PathBuf],
    ) -> dots_providers::Result<std::collections::BTreeMap<std::path::PathBuf, String>> {
        self.0.owner_of(ctx, paths)
    }
}

/// Stands in for a package's script provider when its declaration cannot work (a `fetch`
/// without a `url`): every call fails with `schema-invalid-value` and a `file:line`, so the
/// package fails in plan and apply instead of silently dropping out.
struct Invalid {
    message: String,
}

impl Invalid {
    fn err<T>(&self) -> dots_providers::Result<T> {
        Err(dots_providers::ProviderError::Coded {
            code: "schema-invalid-value",
            message: self.message.clone(),
            log: None,
        })
    }
}

impl dots_providers::Provider for Invalid {
    fn id(&self) -> &'static str {
        "script"
    }
    fn caps(&self) -> dots_providers::Caps {
        dots_providers::Caps {
            batch_install: false,
            exact_version: dots_providers::Support::No,
            pin: false,
            needs_root: false,
            uninstall: false,
            exclusive_lock: false,
        }
    }
    fn availability(&self, _: &Facts) -> dots_providers::Availability {
        dots_providers::Availability::Ready {
            bin_path: "sh".into(),
        }
    }
    fn installed(
        &self,
        _: &Ctx,
    ) -> dots_providers::Result<std::collections::BTreeMap<String, dots_providers::Installed>> {
        self.err()
    }
    fn latest(
        &self,
        _: &Ctx,
        _: &[dots_providers::Spec],
    ) -> dots_providers::Result<std::collections::BTreeMap<String, dots_providers::Latest>> {
        self.err()
    }
    fn plan(
        &self,
        _: &[dots_providers::Op],
    ) -> dots_providers::Result<Vec<dots_providers::Action>> {
        self.err()
    }
    fn exec(
        &self,
        _: &Ctx,
        _: &dots_providers::Action,
    ) -> dots_providers::Result<dots_providers::Outcome> {
        self.err()
    }
    fn bins(
        &self,
        _: &Ctx,
        _: &std::collections::BTreeMap<String, dots_providers::Installed>,
    ) -> dots_providers::Result<Vec<dots_providers::BinEntry>> {
        self.err()
    }
}

/// [`registry`], plus handles on the script instances. A script's `fetch` file is checked
/// against the hash `dots.lock` recorded for it on first use.
pub fn registry_with_handles(
    dirs: &Dirs,
    ctx: &Ctx,
    facts: &Facts,
    repo: Option<&Repo>,
    root: &Path,
    inputs: ScriptInputs<'_>,
) -> (Registry, ScriptHandles) {
    let mut reg = shared_registry(ctx);
    let mut handles = ScriptHandles::default();
    let Some(repo) = repo else {
        return (reg, handles);
    };
    let lock = dots_lock::Lock::load(&root.join(dots_lock::FILE)).unwrap_or_default();
    for (id, table) in &repo.manifest.providers {
        if !enabled(ctx, id) {
            continue;
        }
        if let Ok(def) = CustomDef::from_json(id, table) {
            reg.register(Box::new(
                CustomProvider::new(def, facts).with_probe(ctx.clone()),
            ));
        }
    }
    for (name, pkg) in &repo.packages {
        for cfg in pkg.providers.values() {
            match cfg {
                ProviderConfig::Script(s)
                    if enabled(ctx, "script")
                        && s.fetch.as_ref().is_some_and(|f| f.url.is_none()) =>
                {
                    let file = pkg.dir.join("package.toml").display().to_string();
                    let line = pkg
                        .spans
                        .get("providers.script.fetch")
                        .or_else(|| pkg.spans.get("providers.script"))
                        .map_or_else(|| "1".to_string(), |a| a.0.to_string());
                    reg.register_for(
                        name,
                        Box::new(Invalid {
                            message: format!(
                                "schema-invalid-value: {file}:{line}: `providers.script.fetch` needs a `url`"
                            ),
                        }),
                    );
                }
                ProviderConfig::Script(s) if enabled(ctx, "script") => {
                    let env = ScriptEnv {
                        home: dirs.home.clone(),
                        pkg_dir: pkg.dir.clone(),
                        root: root.to_path_buf(),
                        state: dirs.state.clone(),
                        bin_dir: dirs.home.join(".local/bin"),
                        lib_dir: root.join("lib"),
                        tags: inputs.tags.to_vec(),
                        settings: script_settings(inputs.settings, name),
                        path_prefix: Vec::new(),
                    };
                    // An invalid script table is a lint matter; the package just has no
                    // script provider.
                    if let Ok(p) =
                        ScriptProvider::new(name, script_config(s, pkg.bins.clone()), env, facts)
                    {
                        let pin = lock
                            .content_for(&fetch_item(name))
                            .and_then(|c| c.sha256.clone());
                        let shared = std::sync::Arc::new(p.with_lock_pin(pin));
                        handles.0.insert(name.clone(), shared.clone());
                        reg.register_for(name, Box::new(Shared(shared)));
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
    (reg, handles)
}

/// The lock's `[[content]]` item for the file a script package downloads.
pub fn fetch_item(pkg: &str) -> String {
    format!("fetch:{pkg}")
}

/// One batched `installed()` call per provider instance; a failing provider contributes
/// nothing (the status commands report provider problems themselves).
pub fn installed(registry: &Registry, ctx: &Ctx) -> InstalledMap {
    installed_with_problems(registry, ctx).0
}

/// What the provider queries of one planning pass reported besides the answers.
#[derive(Default)]
pub struct QueryProblems {
    /// A `check` that ran out of time (`query-timeout`): errors for those packages only.
    pub errors: Vec<dots_core::report::Diagnostic>,
    /// A `version` or `latest` that ran out of time: unknown, the plan goes on.
    pub warnings: Vec<dots_core::report::Diagnostic>,
}

impl QueryProblems {
    fn take(errors: Vec<dots_core::report::Diagnostic>) -> Self {
        QueryProblems {
            errors,
            warnings: take_query_warnings(),
        }
    }
}

/// The `version` / `latest` queries that timed out since the last call, as warnings.
pub fn take_query_warnings() -> Vec<dots_core::report::Diagnostic> {
    dots_providers::take_query_timeouts()
        .into_iter()
        .map(|t| {
            dots_core::report::Diagnostic::from_code(
                "version-query-timeout",
                &[
                    ("package", &t.package),
                    ("what", t.what),
                    ("timeout", &t.timeout),
                ],
            )
        })
        .collect()
}

/// [`installed`], also returning the queries that timed out. A hung `check` is not the same
/// as a package that is missing, so it is an error for that package instead of silently
/// planning its install.
pub fn installed_with_problems(registry: &Registry, ctx: &Ctx) -> (InstalledMap, QueryProblems) {
    let mut errors = Vec::new();
    let mut out = InstalledMap::new();
    let ids: Vec<&str> = registry.ids().collect();
    for (i, id) in ids.iter().enumerate() {
        let instances: Vec<_> = registry.instances(id).collect();
        // Per-package instances (script checks) are many cheap commands, shared ones one query.
        let checks = instances.iter().filter(|(pkg, _)| pkg.is_some()).count();
        for (n, (_, p)) in instances.iter().enumerate() {
            dots_ui::phase::note(&if checks > 1 {
                format!("Running {checks} {id} checks ({}/{checks})", n + 1)
            } else {
                format!(
                    "Checking installed packages ({id}, {}/{})",
                    i + 1,
                    ids.len()
                )
            });
            match p.installed(ctx) {
                Ok(m) => out.entry(id.to_string()).or_default().extend(m),
                Err(dots_providers::ProviderError::Coded {
                    code: "query-timeout",
                    message,
                    ..
                }) => {
                    let mut d = dots_core::report::Diagnostic::from_code("query-timeout", &[]);
                    d.message = message;
                    errors.push(d);
                }
                Err(_) => {}
            }
        }
    }
    (out, QueryProblems::take(errors))
}

/// One (provider, package) the resolver, planner or upgrade may ask `offered()` about.
pub struct OfferCandidate {
    pub provider: String,
    pub pkg: String,
    pub spec: dots_providers::Spec,
}

/// What providers offer, asked live and remembered for the run. Backs constraint-aware
/// provider selection and drift notes; a provider that cannot answer offers nothing (and is
/// then assumed able to deliver).
///
/// Asking costs a provider call (`brew info` is about a second per call), so the callers
/// announce the candidates they will ask about ([`LiveOffers::with_candidates`]). The first
/// miss for a shared provider instance then asks `latest()` once for every announced
/// candidate of that instance. Per-package instances (script, git, custom) are asked per
/// package. If the batch fails, each spec is asked on its own so one bad name does not
/// blank the rest.
pub struct LiveOffers<'a> {
    registry: &'a Registry,
    ctx: &'a Ctx,
    candidates: Vec<OfferCandidate>,
    seen: std::cell::RefCell<std::collections::BTreeMap<(String, String), Option<String>>>,
    /// Shared provider ids whose candidates were already fetched.
    fetched: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

impl<'a> LiveOffers<'a> {
    pub fn new(registry: &'a Registry, ctx: &'a Ctx) -> Self {
        Self::with_candidates(registry, ctx, Vec::new())
    }

    pub fn with_candidates(
        registry: &'a Registry,
        ctx: &'a Ctx,
        candidates: Vec<OfferCandidate>,
    ) -> Self {
        LiveOffers {
            registry,
            ctx,
            candidates,
            seen: Default::default(),
            fetched: Default::default(),
        }
    }

    /// Ask one provider instance about `specs`, remembering every answer. One `latest()`
    /// call; when it fails, one per spec.
    fn fetch(
        &self,
        provider: &str,
        p: &dyn dots_providers::Provider,
        specs: &[dots_providers::Spec],
    ) {
        dots_ui::phase::note(&format!(
            "Looking up latest versions ({provider}: {} package{})",
            specs.len(),
            if specs.len() == 1 { "" } else { "s" }
        ));
        let mut seen = self.seen.borrow_mut();
        let mut record =
            |spec: &dots_providers::Spec,
             m: &std::collections::BTreeMap<String, dots_providers::Latest>| {
                seen.insert(
                    (provider.to_string(), spec.name.clone()),
                    m.get(&spec.name).map(|l| l.version.clone()),
                );
            };
        match p.latest(self.ctx, specs) {
            Ok(m) => specs.iter().for_each(|sp| record(sp, &m)),
            Err(_) if specs.len() > 1 => {
                for sp in specs {
                    let m = p
                        .latest(self.ctx, std::slice::from_ref(sp))
                        .unwrap_or_default();
                    record(sp, &m);
                }
            }
            Err(_) => specs.iter().for_each(|sp| record(sp, &Default::default())),
        }
    }
}

impl dots_resolve::OfferSource for LiveOffers<'_> {
    fn offered(
        &self,
        provider: &str,
        pkg: &str,
        native: &str,
        spec: &dots_providers::Spec,
    ) -> Option<String> {
        let key = (provider.to_string(), native.to_string());
        if let Some(v) = self.seen.borrow().get(&key) {
            return v.clone();
        }
        if let Some(p) = self.registry.get_for(provider, pkg) {
            if self.registry.has_scoped(provider, pkg) {
                self.fetch(provider, p, std::slice::from_ref(spec));
            } else {
                // The shared instance: ask for everything announced in one call (plus this
                // spec, when it was not announced).
                let first = self.fetched.borrow_mut().insert(provider.to_string());
                let mut specs: Vec<dots_providers::Spec> = Vec::new();
                if first {
                    for c in self.candidates.iter().filter(|c| {
                        c.provider == provider && !self.registry.has_scoped(provider, &c.pkg)
                    }) {
                        if !specs.iter().any(|s| s.name == c.spec.name) {
                            specs.push(c.spec.clone());
                        }
                    }
                }
                if !specs.iter().any(|s| s.name == native) {
                    specs.push(spec.clone());
                }
                self.fetch(provider, p, &specs);
            }
        }
        let answer = self.seen.borrow().get(&key).cloned().flatten();
        self.seen.borrow_mut().entry(key).or_insert(answer.clone());
        answer
    }
}

/// What the `outdated` cache last saw, for commands that never touch the network.
pub struct CachedOffers {
    cache: std::collections::BTreeMap<String, crate::inspect::CacheFile>,
}

impl CachedOffers {
    /// `cache_dir` is the provider context's cache directory (`Ctx::cache_dir`).
    pub fn new(cache_dir: &Path) -> Self {
        CachedOffers {
            cache: crate::inspect::LatestCache::at(cache_dir.join("latest")).load_all(),
        }
    }
}

impl dots_resolve::OfferSource for CachedOffers {
    fn offered(
        &self,
        provider: &str,
        _: &str,
        native: &str,
        _: &dots_providers::Spec,
    ) -> Option<String> {
        crate::inspect::cached_latest(&self.cache, provider, native).map(str::to_string)
    }
}

#[cfg(test)]
mod home_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// `--home <scratch>` redirects dots' own paths but provider commands keep the real HOME
    /// (`brew info --installed` missed tapped formulae under a scratch HOME).
    #[test]
    fn sandboxed_home_does_not_change_the_providers_home() {
        let dirs = Dirs::resolve(Some(Path::new("/scratch")), &dots_core::ProcessEnv);
        let env = BTreeMap::from([("HOME".to_string(), "/real".to_string())]);
        let ctx = exec_ctx_from(&dirs, env, false, false, "r1");
        assert_eq!(ctx.env["HOME"], "/real");
        assert_eq!(ctx.home, PathBuf::from("/real"));
        assert!(ctx.log_dir.starts_with("/scratch"), "{:?}", ctx.log_dir);
        assert!(ctx.cache_dir.starts_with("/scratch"));
        // No HOME at all: the sandbox stands in.
        let ctx = exec_ctx_from(&dirs, BTreeMap::new(), false, false, "r1");
        assert_eq!(ctx.env["HOME"], "/scratch");
    }
}

#[cfg(test)]
mod offers_tests {
    use super::*;
    use dots_providers::*;
    use dots_resolve::OfferSource;
    use dots_testkit::FakeProvider;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    /// Records every `latest()` call's spec names; a batch naming `bad` errors, like
    /// `brew info` with an unknown name.
    struct Counting {
        inner: FakeProvider,
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }
    impl Provider for Counting {
        fn id(&self) -> &'static str {
            "fake"
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
            self.calls
                .lock()
                .unwrap()
                .push(s.iter().map(|x| x.name.clone()).collect());
            if s.iter().any(|x| x.name == "bad") {
                return Err(ProviderError::Other("unknown".into()));
            }
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> Result<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> Result<Outcome> {
            self.inner.exec(c, a)
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> Result<Vec<BinEntry>> {
            self.inner.bins(c, i)
        }
    }

    fn setup() -> (Registry, Arc<Mutex<Vec<Vec<String>>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut reg = Registry::new();
        reg.register(Box::new(Counting {
            inner: FakeProvider::new("fake")
                .with_latest("a", "1")
                .with_latest("b", "2")
                .with_latest("c", "3"),
            calls: calls.clone(),
        }));
        (reg, calls)
    }

    fn cand(n: &str) -> OfferCandidate {
        OfferCandidate {
            provider: "fake".into(),
            pkg: n.into(),
            spec: Spec::new(n),
        }
    }

    fn ctx() -> Ctx {
        let dirs = Dirs::resolve(Some(Path::new("/scratch")), &dots_core::ProcessEnv);
        exec_ctx_from(&dirs, BTreeMap::new(), false, false, "r1")
    }

    #[test]
    fn announced_candidates_are_asked_in_one_call() {
        let (reg, calls) = setup();
        let ctx = ctx();
        let offers = LiveOffers::with_candidates(&reg, &ctx, vec![cand("a"), cand("b"), cand("c")]);
        for n in ["a", "b", "c", "a"] {
            let v = offers.offered("fake", n, n, &Spec::new(n));
            assert_eq!(
                v.as_deref(),
                Some(match n {
                    "a" => "1",
                    "b" => "2",
                    _ => "3",
                })
            );
        }
        assert_eq!(offers.offered("fake", "zz", "zz", &Spec::new("zz")), None);
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0], ["a", "b", "c"]);
        assert_eq!(
            calls.len(),
            2,
            "one batch, then one for the unannounced name"
        );
    }

    #[test]
    fn failing_batch_falls_back_per_spec() {
        let (reg, calls) = setup();
        let ctx = ctx();
        let offers =
            LiveOffers::with_candidates(&reg, &ctx, vec![cand("a"), cand("bad"), cand("c")]);
        assert_eq!(
            offers.offered("fake", "a", "a", &Spec::new("a")).as_deref(),
            Some("1")
        );
        assert_eq!(
            offers.offered("fake", "bad", "bad", &Spec::new("bad")),
            None
        );
        assert_eq!(
            offers.offered("fake", "c", "c", &Spec::new("c")).as_deref(),
            Some("3")
        );
        // The batch, then one call per spec.
        assert_eq!(calls.lock().unwrap().len(), 4);
    }

    #[test]
    fn offline_offers_nothing() {
        let dirs = Dirs::resolve(Some(Path::new("/scratch")), &dots_core::ProcessEnv);
        let ctx = exec_ctx_from(&dirs, BTreeMap::new(), true, false, "r1");
        let reg = {
            let mut r = Registry::new();
            r.register(Box::new(dots_providers::brew::BrewProvider::new()));
            r
        };
        let offers = LiveOffers::with_candidates(&reg, &ctx, vec![cand("a")]);
        assert_eq!(offers.offered("brew", "a", "a", &Spec::new("a")), None);
    }
}
