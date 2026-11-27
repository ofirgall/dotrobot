//! Reconcile-engine scenarios (tasks 4.1-4.3) against FakeProvider in a `--home` sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use dots_core::{Dirs, MapEnv};
use dots_engine::*;
use dots_facts::{Chain, Facts};
use dots_model::{MachineConfig, Repo};
use dots_providers::{
    Action, Availability, BinEntry, Caps, Ctx, Installed, Latest, Op, Outcome, Provider, Registry,
    Result as PResult, Spec,
};
use dots_resolve::InstalledMap;
use dots_testkit::{FakeProvider, RecordingRunner};

// ------------------------------------------------------------------ harness

struct Sandbox {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    dirs: Dirs,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A repo with the given `(name, package.toml body)` packages and one bundle `all` holding
/// every package (plus `extra` bundles).
fn sandbox(pkgs: &[(&str, &str)], extra_bundles: &[(&str, &[&str])]) -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    for (name, body) in pkgs {
        write(
            &root.join(format!("packages/{name}/package.toml")),
            &format!("schema = 1\nname = \"{name}\"\n{body}\n"),
        );
    }
    let all: Vec<String> = pkgs.iter().map(|(n, _)| format!("\"{n}\"")).collect();
    write(
        &root.join("bundles/all.toml"),
        &format!("schema = 1\npackages = [{}]\n", all.join(", ")),
    );
    for (b, members) in extra_bundles {
        let m: Vec<String> = members.iter().map(|n| format!("\"{n}\"")).collect();
        write(
            &root.join(format!("bundles/{b}.toml")),
            &format!("schema = 1\npackages = [{}]\n", m.join(", ")),
        );
    }
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dirs = Dirs::resolve(Some(&home), &MapEnv::new());
    Sandbox {
        _tmp: tmp,
        root,
        dirs,
    }
}

fn facts() -> Facts {
    Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("debian".into()),
        has_sudo: true,
        ..Default::default()
    }
}

fn installed_map(reg: &Registry, ctx: &Ctx) -> InstalledMap {
    let mut m = InstalledMap::new();
    for id in reg.ids() {
        for (_, p) in reg.instances(id) {
            m.entry(id.to_string())
                .or_default()
                .extend(p.installed(ctx).unwrap());
        }
    }
    m
}

struct Case<'a> {
    sb: &'a Sandbox,
    reg: &'a Registry,
    ctx: Ctx,
    files: &'a dyn FilesPhase,
    settings: &'a dyn SettingsPhase,
    services: &'a dyn ServicesPhase,
    hooks: &'a dyn HooksPhase,
    scope: Scope,
    unreviewed: BTreeSet<String>,
    lock: dots_lock::Lock,
    offers: MapOffers,
    upgrade: BTreeMap<String, dots_engine::UpgradeTarget>,
}

/// What a fake registry "offers", by `(provider, native)`.
#[derive(Default)]
struct MapOffers(BTreeMap<(String, String), String>);

impl dots_resolve::OfferSource for MapOffers {
    fn offered(
        &self,
        provider: &str,
        _pkg: &str,
        native: &str,
        _spec: &dots_providers::Spec,
    ) -> Option<String> {
        self.0
            .get(&(provider.to_string(), native.to_string()))
            .cloned()
    }
}

impl<'a> Case<'a> {
    fn new(sb: &'a Sandbox, reg: &'a Registry, files: &'a dyn FilesPhase) -> Self {
        let run_ctx = Ctx {
            home: sb.dirs.home.clone(),
            log_dir: sb.dirs.logs("r-test"),
            ..Ctx::default()
        };
        Case {
            sb,
            reg,
            ctx: run_ctx,
            files,
            settings: &NoSettingsPhase,
            services: &NoServicesPhase,
            hooks: &NoHooksPhase,
            scope: Scope::default(),
            unreviewed: BTreeSet::new(),
            lock: dots_lock::Lock::default(),
            offers: MapOffers::default(),
            upgrade: BTreeMap::new(),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&Inputs<'_>) -> R) -> R {
        let repo = Repo::load(&self.sb.root)
            .unwrap_or_else(|e| panic!("{:#?}", e.errors))
            .value;
        let machine = MachineConfig {
            bundles: vec!["all".into()],
            ..Default::default()
        };
        let facts = facts();
        let chain = Chain::build(&facts, &BTreeMap::new(), &machine.platform_extra);
        let installed = installed_map(self.reg, &self.ctx);
        let res = dots_resolve::resolve_with(
            &dots_resolve::Inputs {
                repo: &repo,
                machine: &machine,
                facts: &facts,
                chain: &chain,
                providers: self.reg,
                installed: &installed,
            },
            &self.offers,
        );
        assert!(res.is_ok(), "{:#?}", res.errors);
        let locks = dots_engine::LockInputs {
            lock: &self.lock,
            machine_key: "debian/x86_64",
            offers: &self.offers,
            upgrade: &self.upgrade,
        };
        f(&Inputs {
            repo: &repo,
            resolution: &res,
            registry: self.reg,
            installed: &installed,
            files: self.files,
            settings: self.settings,
            services: self.services,
            hooks: self.hooks,
            ctx: &self.ctx,
            dirs: &self.sb.dirs,
            scope: &self.scope,
            unreviewed: &self.unreviewed,
            locks: Some(&locks),
        })
    }

    fn plan(&self) -> Plan {
        self.with(|i| plan(i, "r-test").unwrap())
    }

    fn apply_with(&self, cfg: &ApplyConfig, prompter: &dyn Prompter) -> ApplyReport {
        let lock = RunLock::acquire(&self.sb.dirs.run_lock()).unwrap();
        self.with(|i| apply(i, cfg, prompter, Mode::Live(&lock)))
    }

    fn apply(&self, yes: bool) -> ApplyReport {
        self.apply_with(&cfg(yes), &NoPrompt)
    }
}

fn cfg(yes: bool) -> ApplyConfig {
    ApplyConfig {
        run_id: "r-test".into(),
        yes,
        allow_downgrade: false,
        sudo: false,
        journal: JournalMeta {
            host: "h".into(),
            root: "main".into(),
            dots_version: "0.1.0".into(),
            ..Default::default()
        },
        stop: Arc::new(AtomicBool::new(false)),
    }
}

fn registry(ps: &[&FakeProvider]) -> Registry {
    let mut r = Registry::new();
    for p in ps {
        r.register(Box::new((*p).clone()));
    }
    r
}

fn status_of<'a>(r: &'a ApplyReport, pkg: &str) -> &'a StepStatus {
    &r.steps.iter().find(|s| s.package == pkg).unwrap().status
}

/// A files phase that records what it did and in what order relative to provider executes.
struct FakeFiles {
    steps: Vec<FileStep>,
    log: Arc<Mutex<Vec<String>>>,
    execs_seen: Option<FakeProvider>,
}

fn fstep(pkg: &str, dst: &str, owned: bool) -> FileStep {
    FileStep {
        package: pkg.into(),
        dst: dst.into(),
        kind: "link".into(),
        detail: "-> src".into(),
        owned,
        ..Default::default()
    }
}

impl FakeFiles {
    fn new(steps: Vec<FileStep>) -> Self {
        FakeFiles {
            steps,
            log: Arc::default(),
            execs_seen: None,
        }
    }
}

impl FilesPhase for FakeFiles {
    fn plan(&self, req: &FilesRequest<'_>) -> Result<Vec<FileStep>, String> {
        Ok(self
            .steps
            .iter()
            .filter(|s| req.packages.contains(&s.package))
            .cloned()
            .collect())
    }

    fn apply(
        &self,
        req: &FilesRequest<'_>,
        steps: &[FileStep],
        done: &mut dyn FnMut(FileStepResult),
    ) -> Result<(), String> {
        assert!(!req.dry_run);
        for s in steps {
            let seen = self.execs_seen.as_ref().map_or(0, |p| p.executed().len());
            self.log.lock().unwrap().push(format!("{}@{seen}", s.dst));
            done(FileStepResult {
                step: s.clone(),
                ok: true,
                detail: None,
                dur_ms: 1,
            });
        }
        Ok(())
    }
}

fn read_journal(sb: &Sandbox) -> Vec<serde_json::Value> {
    std::fs::read_to_string(sb.dirs.journal())
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn read_state(sb: &Sandbox) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(sb.dirs.state_json()).unwrap()).unwrap()
}

const PKGS: &[(&str, &str)] = &[
    ("fd", "[providers.brew]"),
    ("bat", "[providers.brew]"),
    ("rg", "[providers.cargo]"),
];

// ------------------------------------------------------------------ 4.1

#[test]
fn plan_then_apply_performs_exactly_the_planned_actions() {
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let case = Case::new(&sb, &reg, &NoFiles);

    let p = case.plan();
    assert_eq!(p.steps.len(), 3);
    // Batched per provider: one brew action carrying both packages, one cargo action.
    let planned: BTreeSet<Vec<String>> = p
        .batches
        .iter()
        .flat_map(|b| b.actions.iter().map(|a| a.argv.clone()))
        .collect();
    assert_eq!(planned.len(), 2);
    // Planning has no side effects.
    assert!(brew.executed().is_empty() && cargo.executed().is_empty());

    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    let ran: BTreeSet<Vec<String>> = brew
        .executed()
        .into_iter()
        .chain(cargo.executed())
        .map(|a| a.argv)
        .collect();
    assert_eq!(ran, planned);
    assert!(brew.installed_calls() > 0);
}

#[test]
fn second_apply_is_a_no_op() {
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let case = Case::new(&sb, &reg, &NoFiles);
    assert_eq!(case.apply(true).status, Status::Done);
    let before = brew.executed().len() + cargo.executed().len();
    assert!(case.plan().is_empty());
    let r = case.apply(false);
    assert_eq!(r.status, Status::Done);
    assert!(r.steps.is_empty());
    assert_eq!(brew.executed().len() + cargo.executed().len(), before);
}

/// An installed package the provider calls stale (an apt repository not configured yet) gets an
/// upgrade step with the reason; once the upgrade ran, the next plan is empty.
#[test]
fn stale_installed_package_is_upgraded_once() {
    let sb = sandbox(&[("fd", "[providers.brew]")], &[]);
    let brew = FakeProvider::new("brew")
        .with_installed("fd", "9.0.0")
        .with_stale("fd", "repo x/y not configured");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let p = case.plan();
    let step = p.step("fd").expect("an upgrade step");
    assert_eq!(step.kind, StepKind::Upgrade);
    assert_eq!(step.note.as_deref(), Some("repo x/y not configured"));
    assert_eq!(case.apply(true).status, Status::Done);
    assert!(case.plan().is_empty());
    // Not installed at all: an install, not an upgrade, and no note.
    let brew = FakeProvider::new("brew").with_stale("fd", "repo x/y not configured");
    let reg = registry(&[&brew]);
    let p = Case::new(&sb, &reg, &NoFiles).plan();
    assert_eq!(p.step("fd").unwrap().kind, StepKind::Install);
    assert_eq!(p.step("fd").unwrap().note, None);
}

#[test]
fn dry_run_executes_nothing_and_needs_no_lock() {
    let sb = sandbox(PKGS, &[]);
    let brew = FakeProvider::new("brew");
    let cargo = FakeProvider::new("cargo");
    let reg = registry(&[&brew, &cargo]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.with(|i| apply(i, &cfg(true), &NoPrompt, Mode::DryRun));
    assert_eq!(r.status, Status::DryRun);
    assert_eq!(r.plan.steps.len(), 3);
    assert!(brew.executed().is_empty() && cargo.executed().is_empty());
    assert!(!sb.dirs.state_json().exists());
}

#[test]
fn requires_orders_installs_across_batches() {
    let sb = sandbox(
        &[
            ("base", "[providers.brew]"),
            ("app", "requires = [\"base\"]\n[providers.brew]"),
        ],
        &[],
    );
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let p = case.plan();
    assert_eq!(p.batches.len(), 2, "dependent install is its own batch");
    assert_eq!(p.step("app").unwrap().deps, vec!["base".to_string()]);
    case.apply(true);
    let order: Vec<String> = brew.executed().iter().map(|a| a.argv.join(" ")).collect();
    assert!(
        order[0].contains("base") && order[1].contains("app"),
        "{order:?}"
    );
}

#[test]
fn config_after_install() {
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let mut files = FakeFiles::new(vec![
        fstep("fd", "~/.fdrc", false),
        fstep("rg", "~/.rgrc", false),
    ]);
    files.execs_seen = Some(brew.clone());
    let case = Case::new(&sb, &reg, &files);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done);
    // brew had already executed (its one batched action) when the links were made.
    let log = files.log.lock().unwrap().clone();
    assert_eq!(log, vec!["~/.fdrc@1", "~/.rgrc@1"]);
    assert_eq!(r.files.len(), 2);
}

#[test]
fn files_of_a_failed_package_are_not_applied() {
    let sb = sandbox(PKGS, &[]);
    let brew = FakeProvider::new("brew").failing_on("fd");
    let cargo = FakeProvider::new("cargo");
    let reg = registry(&[&brew, &cargo]);
    let files = FakeFiles::new(vec![
        fstep("fd", "~/.fdrc", false),
        fstep("rg", "~/.rgrc", false),
    ]);
    let case = Case::new(&sb, &reg, &files);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(files.log.lock().unwrap().clone(), vec!["~/.rgrc@0"]);
    assert_eq!(r.files_skipped.len(), 1);
    assert_eq!(r.files_skipped[0].0.dst, "~/.fdrc");
}

#[test]
fn independent_providers_run_in_parallel() {
    // Each exec waits (up to 5s) for the other to be inside exec too.
    struct Rendezvous {
        inner: FakeProvider,
        gate: Arc<(Mutex<usize>, Condvar)>,
        met: Arc<AtomicBool>,
    }
    impl Provider for Rendezvous {
        fn id(&self) -> &'static str {
            self.inner.id()
        }
        fn caps(&self) -> Caps {
            self.inner.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.inner.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.inner.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            let (m, cv) = &*self.gate;
            let mut n = m.lock().unwrap();
            *n += 1;
            cv.notify_all();
            let (n, t) = cv
                .wait_timeout_while(n, Duration::from_secs(5), |n| *n < 2)
                .unwrap();
            if *n >= 2 && !t.timed_out() {
                self.met.store(true, Ordering::SeqCst);
            }
            drop(n);
            self.inner.exec(c, a)
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.inner.bins(c, i)
        }
    }
    let sb = sandbox(PKGS, &[]);
    let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
    let met = Arc::new(AtomicBool::new(false));
    let mut reg = Registry::new();
    for id in ["brew", "cargo"] {
        reg.register(Box::new(Rendezvous {
            inner: FakeProvider::new(id),
            gate: gate.clone(),
            met: met.clone(),
        }));
    }
    let case = Case::new(&sb, &reg, &NoFiles);
    assert_eq!(case.apply(true).status, Status::Done);
    assert!(
        met.load(Ordering::SeqCst),
        "brew and cargo never ran at the same time"
    );
}

/// A script (or custom) batch never overlaps a root package manager batch: scripts call
/// `apt-get` themselves and hit the dpkg lock (`Could not get lock /var/lib/dpkg/lock-frontend`).
#[test]
fn script_batches_never_overlap_an_apt_batch() {
    struct Probe {
        inner: FakeProvider,
        /// `(apt or script batches in flight now, the most seen at once)`
        flight: Arc<Mutex<(usize, usize)>>,
    }
    impl Provider for Probe {
        fn id(&self) -> &'static str {
            self.inner.id()
        }
        fn caps(&self) -> Caps {
            self.inner.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.inner.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.inner.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            let id = self.inner.id();
            let counted = id != "cargo";
            if counted {
                let mut f = self.flight.lock().unwrap();
                f.0 += 1;
                f.1 = f.1.max(f.0);
            }
            std::thread::sleep(Duration::from_millis(300));
            let r = self.inner.exec(c, a);
            if counted {
                self.flight.lock().unwrap().0 -= 1;
            }
            r
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.inner.bins(c, i)
        }
    }
    let sb = sandbox(
        &[
            ("libfoo", "[providers.apt]"),
            (
                "tool",
                "[providers.script]\ncheck = \"true\"\ninstall = \"true\"\nversionless = true",
            ),
            ("rg", "[providers.cargo]"),
        ],
        &[],
    );
    let flight = Arc::new(Mutex::new((0usize, 0usize)));
    let mut reg = Registry::new();
    let apt = Caps {
        needs_root: true,
        exclusive_lock: true,
        ..FakeProvider::new("x").caps()
    };
    for (id, caps) in [("apt", Some(apt)), ("script", None), ("cargo", None)] {
        let mut inner = FakeProvider::new(id);
        if let Some(c) = caps {
            inner = inner.with_caps(c);
        }
        reg.register(Box::new(Probe {
            inner,
            flight: flight.clone(),
        }));
    }
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:?}", r.errors);
    assert_eq!(r.steps.len(), 3);
    let (_, max) = *flight.lock().unwrap();
    assert_eq!(max, 1, "an apt batch and a script batch overlapped");
}

/// Script packages that name the same `lock` never run at the same time (three `pi install`
/// calls rewriting one `settings.json`); packages without the key still run in parallel.
#[test]
fn script_packages_with_the_same_lock_key_never_overlap() {
    struct Locked {
        inner: FakeProvider,
        key: Option<&'static str>,
        /// `(batches in flight now, the most seen at once)`
        flight: Arc<Mutex<(usize, usize)>>,
    }
    impl Provider for Locked {
        fn id(&self) -> &'static str {
            self.inner.id()
        }
        fn caps(&self) -> Caps {
            self.inner.caps()
        }
        fn lock_key(&self) -> Option<String> {
            self.key.map(String::from)
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.inner.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.inner.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            {
                let mut f = self.flight.lock().unwrap();
                f.0 += 1;
                f.1 = f.1.max(f.0);
            }
            std::thread::sleep(Duration::from_millis(200));
            let r = self.inner.exec(c, a);
            self.flight.lock().unwrap().0 -= 1;
            r
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.inner.bins(c, i)
        }
    }
    let body = "[providers.script]\ncheck = \"true\"\ninstall = \"true\"\nversionless = true";
    let run = |key: Option<&'static str>| {
        let sb = sandbox(&[("p1", body), ("p2", body), ("p3", body)], &[]);
        let flight = Arc::new(Mutex::new((0usize, 0usize)));
        let mut reg = Registry::new();
        for p in ["p1", "p2", "p3"] {
            reg.register_for(
                p,
                Box::new(Locked {
                    inner: FakeProvider::new("script"),
                    key,
                    flight: flight.clone(),
                }),
            );
        }
        let case = Case::new(&sb, &reg, &NoFiles);
        let r = case.apply(true);
        assert_eq!(r.status, Status::Done, "{:?}", r.errors);
        flight.lock().map(|f| f.1).unwrap()
    };
    assert_eq!(run(Some("pi")), 1, "same key: serialized");
    assert!(run(None) > 1, "no key: the script batches run in parallel");
}

/// One bad package name fails its own install, not the batch's other packages: the provider
/// names per-package actions to retry with.
#[test]
fn a_failed_batch_is_retried_per_package_when_the_provider_offers_it() {
    struct Each(FakeProvider);
    impl Provider for Each {
        fn id(&self) -> &'static str {
            self.0.id()
        }
        fn caps(&self) -> Caps {
            self.0.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.0.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.0.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.0.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.0.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            self.0.exec(c, a)
        }
        fn retry_each(&self, a: &Action) -> Option<Vec<Action>> {
            let mut out = Vec::new();
            for op in &a.ops {
                out.extend(self.0.plan(std::slice::from_ref(op)).ok()?);
            }
            Some(out)
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.0.bins(c, i)
        }
    }
    let pkgs: &[(&str, &str)] = &[
        ("good1", "[providers.brew]"),
        ("bad", "[providers.brew]"),
        ("good2", "[providers.brew]"),
    ];
    // Without the retry the whole batch fails together (the default).
    let sb = sandbox(pkgs, &[]);
    let plain = FakeProvider::new("brew").failing_on("bad");
    let reg = registry(&[&plain]);
    let r = Case::new(&sb, &reg, &NoFiles).apply(true);
    assert_eq!(status_of(&r, "good1"), &StepStatus::Failed);

    let sb = sandbox(pkgs, &[]);
    let brew = FakeProvider::new("brew").failing_on("bad");
    let mut reg = Registry::new();
    reg.register(Box::new(Each(brew.clone())));
    let r = Case::new(&sb, &reg, &NoFiles).apply(true);
    assert_eq!(status_of(&r, "good1"), &StepStatus::Ok, "{:?}", r.errors);
    assert_eq!(status_of(&r, "good2"), &StepStatus::Ok);
    assert_eq!(status_of(&r, "bad"), &StepStatus::Failed);
    // One batch, then one install per package.
    assert_eq!(brew.executed().len(), 4);
    assert_eq!(r.status, Status::Failed);
    let after = brew.snapshot_installed();
    assert!(
        after.contains_key("good1") && after.contains_key("good2") && !after.contains_key("bad")
    );

    // A batch that succeeds is one call: no retry, no extra cost.
    let sb = sandbox(pkgs, &[]);
    let brew = FakeProvider::new("brew");
    let mut reg = Registry::new();
    reg.register(Box::new(Each(brew.clone())));
    let r = Case::new(&sb, &reg, &NoFiles).apply(true);
    assert_eq!(r.status, Status::Done);
    assert_eq!(brew.executed().len(), 1);
}

/// `brew install a b c d` stops at the failing package. What it installed before stopping is
/// kept (not reported failed), and the packages it never reached are retried one by one, so
/// only the package that really fails is a failure (with its own log).
#[test]
fn a_batch_that_stopped_part_way_keeps_what_installed_and_retries_the_rest() {
    struct Each(FakeProvider);
    impl Provider for Each {
        fn id(&self) -> &'static str {
            self.0.id()
        }
        fn caps(&self) -> Caps {
            self.0.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.0.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.0.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.0.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.0.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            self.0.exec(c, a)
        }
        fn retry_each(&self, a: &Action) -> Option<Vec<Action>> {
            let mut out = Vec::new();
            for op in &a.ops {
                out.extend(self.0.plan(std::slice::from_ref(op)).ok()?);
            }
            Some(out)
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.0.bins(c, i)
        }
    }
    // Planned in name order: a, b, bad, d, e. The command stops at `bad`.
    let pkgs: &[(&str, &str)] = &[
        ("a", "[providers.brew]"),
        ("b", "[providers.brew]"),
        ("bad", "[providers.brew]"),
        ("d", "[providers.brew]"),
        ("e", "[providers.brew]"),
    ];
    let sb = sandbox(pkgs, &[]);
    let brew = FakeProvider::new("brew")
        .failing_on("bad")
        .stopping_at_failures();
    let mut reg = Registry::new();
    reg.register(Box::new(Each(brew.clone())));
    let r = Case::new(&sb, &reg, &NoFiles).apply(true);
    for ok in ["a", "b", "d", "e"] {
        assert_eq!(status_of(&r, ok), &StepStatus::Ok, "{ok}: {:?}", r.errors);
    }
    assert_eq!(status_of(&r, "bad"), &StepStatus::Failed);
    assert_eq!(r.status, Status::Failed);
    // The batch, then one retry for each package that was not installed (bad, d, e).
    let ran: Vec<String> = brew.executed().iter().map(|a| a.argv.join(" ")).collect();
    assert_eq!(ran.len(), 4, "{ran:?}");
    assert_eq!(
        ran[1..],
        ["brew install bad", "brew install d", "brew install e"]
    );
    // One error, for the package that failed.
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(r.errors[0].message.contains("bad"));
}

/// Without `retry_each` the packages the command installed are still kept; only the missing
/// ones fail with the batch, and the one command's error is reported once.
#[test]
fn a_batch_without_retry_still_keeps_what_installed() {
    let pkgs: &[(&str, &str)] = &[
        ("a", "[providers.brew]"),
        ("bad", "[providers.brew]"),
        ("c", "[providers.brew]"),
    ];
    let sb = sandbox(pkgs, &[]);
    let brew = FakeProvider::new("brew")
        .failing_on("bad")
        .stopping_at_failures();
    let reg = registry(&[&brew]);
    let r = Case::new(&sb, &reg, &NoFiles).apply(true);
    assert_eq!(status_of(&r, "a"), &StepStatus::Ok, "{:?}", r.errors);
    assert_eq!(status_of(&r, "bad"), &StepStatus::Failed);
    assert_eq!(status_of(&r, "c"), &StepStatus::Failed);
    assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
    assert!(r.errors[0].message.contains("bad") && r.errors[0].message.contains("c"));
}

// ------------------------------------------------------------------ 4.2

fn apt_caps() -> Caps {
    Caps {
        needs_root: true,
        ..FakeProvider::new("x").caps()
    }
}

#[test]
fn file_only_plan_needs_no_confirmation() {
    let sb = sandbox(PKGS, &[]);
    let brew = FakeProvider::new("brew")
        .with_installed("fd", "1.0.0")
        .with_installed("bat", "1.0.0");
    let cargo = FakeProvider::new("cargo").with_installed("rg", "1.0.0");
    let reg = registry(&[&brew, &cargo]);
    let owned = FakeFiles::new(vec![fstep("fd", "~/.fdrc", true)]);
    let case = Case::new(&sb, &reg, &owned);
    let r = case.apply(false); // no --yes, cannot prompt
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert_eq!(r.files.len(), 1);

    // A file dots does not own yet is a real change: it asks.
    let fresh = FakeFiles::new(vec![fstep("fd", "~/.fdrc", false)]);
    let case = Case::new(&sb, &reg, &fresh);
    let r = case.apply(false);
    assert_eq!(r.status, Status::ConfirmationRequired);
    assert!(fresh.log.lock().unwrap().is_empty());
}

#[test]
fn several_apt_installs_prompt_for_sudo_once() {
    let sb = sandbox(
        &[
            ("a", "[providers.apt]"),
            ("b", "[providers.apt]"),
            ("c", "requires = [\"a\"]\n[providers.apt]"),
            ("ppa-tool", "requires = [\"b\"]\n[providers.apt]"),
        ],
        &[],
    );
    let apt = FakeProvider::new("apt").with_caps(apt_caps());
    let reg = registry(&[&apt]);
    let rec = Arc::new(RecordingRunner::new());
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.ctx.runner = rec.clone();
    let mut c = cfg(true);
    c.sudo = true;
    let r = case.apply_with(&c, &NoPrompt);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    // (`list` calls are the harness querying installed state.)
    let lines: Vec<String> = rec
        .lines()
        .into_iter()
        .filter(|l| !l.ends_with(" list"))
        .collect();
    let primes = lines.iter().filter(|l| *l == "/usr/bin/sudo -v").count();
    assert_eq!(primes, 1, "{lines:?}");
    let installs: Vec<&String> = lines.iter().filter(|l| l.contains(" install ")).collect();
    assert!(
        installs.len() >= 2 && installs.iter().all(|l| l.starts_with("/usr/bin/sudo -n ")),
        "{lines:?}"
    );
    // The first thing the run does is the prompt.
    assert_eq!(lines[0], "/usr/bin/sudo -v");
}

#[test]
fn a_failed_sudo_prime_fails_root_actions_only() {
    let sb = sandbox(&[("a", "[providers.apt]"), ("b", "[providers.brew]")], &[]);
    let apt = FakeProvider::new("apt").with_caps(apt_caps());
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&apt, &brew]);
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &["/usr/bin/sudo", "-v"],
        dots_providers::Output::fail(1, "no tty"),
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.ctx.runner = rec.clone();
    let mut c = cfg(true);
    c.sudo = true;
    let r = case.apply_with(&c, &NoPrompt);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(status_of(&r, "a"), &StepStatus::Failed);
    assert_eq!(status_of(&r, "b"), &StepStatus::Ok);
    assert!(r.errors.iter().any(|e| e.code == "sudo-failed"));
}

fn interactive_repo() -> Sandbox {
    sandbox(
        &[
            ("fd", "[providers.brew]"),
            (
                "wizard",
                "[providers.script]\ncheck = \"false\"\ninstall = \"./wizard.sh\"\ninteractive = true",
            ),
        ],
        &[],
    )
}

#[test]
fn unattended_run_turns_interactive_steps_into_todos() {
    let sb = interactive_repo();
    let (brew, script) = (FakeProvider::new("brew"), FakeProvider::new("script"));
    let reg = registry(&[&brew, &script]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert!(
        script.executed().is_empty(),
        "interactive script must not run"
    );
    assert_eq!(status_of(&r, "wizard"), &StepStatus::Deferred);
    assert_eq!(r.todos.len(), 1);
    let st = read_state(&sb);
    assert_eq!(st["todos"][0]["package"], "wizard");
    assert_eq!(st["todos"][0]["reason"], "interactive");
    assert_eq!(brew.executed().len(), 1);
}

struct Yes(Mutex<Vec<String>>);
impl Prompter for Yes {
    fn can_prompt(&self) -> bool {
        true
    }
    fn confirm(&self, text: &str) -> bool {
        self.0.lock().unwrap().push(text.to_string());
        true
    }
}

#[test]
fn interactive_steps_run_last_one_at_a_time_with_a_prompt() {
    let sb = interactive_repo();
    let (brew, script) = (FakeProvider::new("brew"), FakeProvider::new("script"));
    let reg = registry(&[&brew, &script]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let p = Yes(Mutex::default());
    let r = case.apply_with(&cfg(false), &p);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    let asked = p.0.lock().unwrap().clone();
    assert_eq!(
        asked.len(),
        2,
        "the plan, then the interactive step: {asked:?}"
    );
    assert!(asked[0].contains("Packages") && asked[1].contains("wizard"));
    assert_eq!(script.executed().len(), 1);
    assert!(r.plan.batches.last().unwrap().interactive);
}

#[test]
fn non_interactive_run_fails_fast_with_confirmation_required() {
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(false);
    assert_eq!(r.status, Status::ConfirmationRequired);
    assert_eq!(r.errors[0].code, "confirmation-required");
    assert!(r.errors[0].hint.contains("--yes"));
    assert!(brew.executed().is_empty() && cargo.executed().is_empty());
    assert!(!sb.dirs.state_json().exists());
}

#[test]
fn downgrades_ask_even_with_yes_unless_allowed() {
    let sb = sandbox(&[("fd", "version = \"1.0.0\"\n[providers.brew]")], &[]);
    let brew = FakeProvider::new("brew").with_installed("fd", "2.0.0");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.status, Status::ConfirmationRequired);
    assert!(r.errors[0].message.contains("downgraded"));
    assert!(brew.executed().is_empty());
    // An interactive user is asked even though --yes was passed.
    let p = Yes(Mutex::default());
    assert_eq!(case.apply_with(&cfg(true), &p).status, Status::Done);
    assert_eq!(p.0.lock().unwrap().len(), 1);
    // --allow-downgrade removes the question.
    let brew = FakeProvider::new("brew").with_installed("fd", "2.0.0");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let mut c = cfg(true);
    c.allow_downgrade = true;
    assert_eq!(case.apply_with(&c, &NoPrompt).status, Status::Done);
    assert_eq!(read_journal(&sb)[0]["op"], "downgrade");
}

#[test]
fn scripts_from_unreviewed_sources_always_ask() {
    let sb = sandbox(
        &[(
            "tool",
            "[providers.script]\ncheck = \"false\"\ninstall = \"x\"",
        )],
        &[],
    );
    let script = FakeProvider::new("script");
    let reg = registry(&[&script]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.unreviewed.insert("tool".into());
    let r = case.apply(true);
    assert_eq!(r.status, Status::ConfirmationRequired);
    assert!(r.errors[0].message.contains("not reviewed"));
    assert!(script.executed().is_empty());
}

#[test]
fn a_declined_plan_changes_nothing() {
    struct No;
    impl Prompter for No {
        fn can_prompt(&self) -> bool {
            true
        }
        fn confirm(&self, _: &str) -> bool {
            false
        }
    }
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply_with(&cfg(false), &No);
    assert_eq!(r.status, Status::Declined);
    assert!(brew.executed().is_empty());
}

// ------------------------------------------------------------------ 4.3

#[test]
fn one_failure_does_not_stop_independent_packages() {
    let sb = sandbox(
        &[
            ("tool", "[providers.cargo]"),
            ("dependent", "requires = [\"tool\"]\n[providers.brew]"),
            ("fd", "[providers.brew]"),
            ("bat", "[providers.brew]"),
        ],
        &[],
    );
    let cargo = FakeProvider::new("cargo").failing_on("tool");
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&cargo, &brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(status_of(&r, "tool"), &StepStatus::Failed);
    assert_eq!(status_of(&r, "fd"), &StepStatus::Ok);
    assert_eq!(status_of(&r, "bat"), &StepStatus::Ok);
    assert_eq!(
        status_of(&r, "dependent"),
        &StepStatus::SkippedDueToDep {
            dependency: "tool".into()
        }
    );
    assert!(
        r.errors
            .iter()
            .any(|e| e.code == "action-failed" && e.message.contains("tool"))
    );
    assert!(r.warnings.iter().any(|w| w.code == "skipped-due-to-dep"));
    // The failed package's log tail is in the summary.
    let failed = r.steps.iter().find(|s| s.package == "tool").unwrap();
    assert!(
        failed.error.as_ref().unwrap().contains("failed to install"),
        "{:?}",
        failed.error
    );
    let journal = read_journal(&sb);
    assert_eq!(journal.iter().filter(|l| l["exit"] == 1).count(), 1);
    assert!(read_state(&sb)["packages"].get("tool").is_none());
    assert!(read_state(&sb)["packages"].get("fd").is_some());
}

#[test]
fn concurrent_apply_exits_lock_busy_naming_the_holder() {
    let sb = sandbox(PKGS, &[]);
    let first = RunLock::acquire(&sb.dirs.run_lock()).unwrap();
    let err = RunLock::acquire(&sb.dirs.run_lock()).unwrap_err();
    assert_eq!(err.code, "lock-busy");
    assert!(
        err.message.contains(&std::process::id().to_string()),
        "{}",
        err.message
    );
    drop(first);
    assert!(
        RunLock::acquire(&sb.dirs.run_lock()).is_ok(),
        "released with the holder"
    );
}

#[test]
fn interrupted_run_stops_after_the_current_action_with_consistent_state() {
    struct StopAfterFirst {
        inner: FakeProvider,
        stop: Arc<AtomicBool>,
    }
    impl Provider for StopAfterFirst {
        fn id(&self) -> &'static str {
            self.inner.id()
        }
        fn caps(&self) -> Caps {
            self.inner.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.inner.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.inner.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            let out = self.inner.exec(c, a);
            self.stop.store(true, Ordering::SeqCst); // "Ctrl-C" during the first action
            out
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.inner.bins(c, i)
        }
    }
    let sb = sandbox(
        &[
            ("a", "[providers.brew]"),
            ("b", "requires = [\"a\"]\n[providers.brew]"),
            ("c", "requires = [\"b\"]\n[providers.brew]"),
        ],
        &[],
    );
    let brew = FakeProvider::new("brew");
    let c = cfg(true);
    let mut reg = Registry::new();
    reg.register(Box::new(StopAfterFirst {
        inner: brew.clone(),
        stop: c.stop.clone(),
    }));
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply_with(&c, &NoPrompt);
    assert_eq!(r.status, Status::Interrupted);
    assert_eq!(status_of(&r, "a"), &StepStatus::Ok);
    assert_eq!(status_of(&r, "b"), &StepStatus::NotRun);
    assert_eq!(brew.executed().len(), 1);
    let st = read_state(&sb);
    assert_eq!(st["schema"], 1);
    let owned: Vec<&String> = st["packages"].as_object().unwrap().keys().collect();
    assert_eq!(owned, vec!["a"]);
}

#[test]
fn state_is_written_after_each_action() {
    // The provider inspects state.json from inside exec of the second batch.
    struct Peek {
        inner: FakeProvider,
        state: PathBuf,
        seen: Arc<Mutex<Vec<String>>>,
    }
    impl Provider for Peek {
        fn id(&self) -> &'static str {
            self.inner.id()
        }
        fn caps(&self) -> Caps {
            self.inner.caps()
        }
        fn availability(&self, f: &Facts) -> Availability {
            self.inner.availability(f)
        }
        fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
            self.inner.installed(c)
        }
        fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
            self.inner.latest(c, s)
        }
        fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
            self.inner.plan(o)
        }
        fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
            let text = std::fs::read_to_string(&self.state).unwrap_or_default();
            self.seen.lock().unwrap().push(text);
            self.inner.exec(c, a)
        }
        fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
            self.inner.bins(c, i)
        }
    }
    let sb = sandbox(
        &[
            ("a", "[providers.brew]"),
            ("b", "requires = [\"a\"]\n[providers.brew]"),
        ],
        &[],
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut reg = Registry::new();
    reg.register(Box::new(Peek {
        inner: FakeProvider::new("brew"),
        state: sb.dirs.state_json(),
        seen: seen.clone(),
    }));
    let case = Case::new(&sb, &reg, &NoFiles);
    assert_eq!(case.apply(true).status, Status::Done);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].is_empty());
    assert!(
        seen[1].contains("\"a\"") && !seen[1].contains("\"b\""),
        "{}",
        seen[1]
    );
    // No temp files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&sb.dirs.state)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn applying_one_package_includes_its_requirements_only() {
    let sb = sandbox(
        &[
            ("dep", "[providers.brew]"),
            ("ghostty", "requires = [\"dep\"]\n[providers.brew]"),
            ("other", "[providers.brew]"),
        ],
        &[("term", &["ghostty"])],
    );
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&brew]);
    let files = FakeFiles::new(vec![
        fstep("ghostty", "~/.config/ghostty", false),
        fstep("other", "~/.other", false),
    ]);
    let mut case = Case::new(&sb, &reg, &files);
    case.scope.targets = vec!["ghostty".into()];
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done);
    let names: Vec<&str> = r.steps.iter().map(|s| s.package.as_str()).collect();
    assert!(names.contains(&"dep") && names.contains(&"ghostty") && !names.contains(&"other"));
    assert_eq!(files.log.lock().unwrap().len(), 1);

    // A bundle selects its members.
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&brew]);
    let mut case = Case::new(&sb, &reg, &files);
    case.scope.targets = vec!["term".into()];
    assert!(case.plan().step("ghostty").is_some());

    // Unknown names are errors.
    case.scope.targets = vec!["nope".into()];
    let e = case.with(|i| plan(i, "r").unwrap_err());
    assert_eq!(e[0].code, "unknown-package");
}

#[test]
fn files_only_and_packages_only_limit_the_plan() {
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let files = FakeFiles::new(vec![fstep("fd", "~/.fdrc", false)]);
    let mut case = Case::new(&sb, &reg, &files);
    case.scope.files_only = true;
    let p = case.plan();
    assert!(p.steps.is_empty() && p.files.len() == 1);
    case.scope.files_only = false;
    case.scope.packages_only = true;
    let p = case.plan();
    assert!(p.files.is_empty() && p.steps.len() == 3);
}

#[test]
fn journal_has_one_line_per_action_with_the_same_run() {
    let sb = sandbox(PKGS, &[]);
    let (brew, cargo) = (FakeProvider::new("brew"), FakeProvider::new("cargo"));
    let reg = registry(&[&brew, &cargo]);
    let files = FakeFiles::new(vec![fstep("fd", "~/.fdrc", false)]);
    let case = Case::new(&sb, &reg, &files);
    assert_eq!(case.apply(true).status, Status::Done);
    let lines = read_journal(&sb);
    assert_eq!(lines.len(), 4, "3 packages + 1 file: {lines:?}");
    for l in &lines {
        for k in [
            "ts",
            "run",
            "host",
            "op",
            "pkg",
            "provider",
            "from",
            "to",
            "root",
            "repo_rev",
            "repo_dirty",
            "upstream_rev",
            "dots",
            "exit",
            "dur_ms",
            "log",
        ] {
            assert!(l.get(k).is_some(), "missing {k} in {l}");
        }
        assert_eq!(l["run"], "r-test");
    }
    assert_eq!(lines.iter().filter(|l| l["op"] == "install").count(), 3);
    assert_eq!(lines.iter().filter(|l| l["op"] == "link").count(), 1);
    // A package log path is relative to the state dir.
    let l = lines.iter().find(|l| l["pkg"] == "rg").unwrap();
    assert!(
        l["log"].as_str().unwrap().starts_with("logs/r-test/"),
        "{l}"
    );
    let st = read_state(&sb);
    assert!(st["files"]["~/.fdrc"].is_object());
}

// ------------------------------------------------------------------ 5.14 / 5.15: phases

const BREW: &str = "[providers.brew]";

fn installed_brew(names: &[&str]) -> FakeProvider {
    let mut p = FakeProvider::new("brew");
    for n in names {
        p = p.with_installed(n, "1.0.0");
    }
    p
}

fn sstep(pkg: &str, id: &str, restart: &[&str], requires: &[&str], root: bool) -> SettingStep {
    SettingStep {
        package: pkg.into(),
        id: id.into(),
        label: id.into(),
        kind: "defaults".into(),
        action: "set".into(),
        detail: "6 -> 2".into(),
        root,
        restart: restart.iter().map(|s| s.to_string()).collect(),
        requires: requires.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn vstep(pkg: &str, name: &str, action: &str) -> ServiceStep {
    ServiceStep {
        package: pkg.into(),
        name: name.into(),
        manager: "systemd-user".into(),
        action: action.into(),
        detail: "not running".into(),
        commands: vec![format!("systemctl --user {action} {name}")],
        ..Default::default()
    }
}

/// Settings phase that logs into a shared journal of the whole run.
struct FakeSettings {
    steps: Vec<SettingStep>,
    log: Arc<Mutex<Vec<String>>>,
    fail: Vec<String>,
    sudo_seen: Mutex<Vec<bool>>,
    /// What `settled` reports: `(package, reason)` of the todos to close.
    settled: Mutex<Vec<(String, String)>>,
}

impl FakeSettings {
    fn new(steps: Vec<SettingStep>, log: Arc<Mutex<Vec<String>>>) -> Self {
        FakeSettings {
            steps,
            log,
            fail: vec![],
            sudo_seen: Mutex::default(),
            settled: Mutex::default(),
        }
    }
}

impl SettingsPhase for FakeSettings {
    fn plan(&self, req: &SettingsRequest<'_>) -> Result<Vec<SettingStep>, PlanError> {
        assert!(req.dry_run);
        Ok(self
            .steps
            .iter()
            .filter(|s| req.packages.contains(&s.package))
            .cloned()
            .collect())
    }

    fn apply(
        &self,
        req: &SettingsRequest<'_>,
        steps: &[SettingStep],
        done: &mut dyn FnMut(SettingStepResult),
    ) -> Result<(), String> {
        assert!(!req.dry_run);
        self.sudo_seen.lock().unwrap().push(req.sudo);
        for s in steps {
            self.log.lock().unwrap().push(format!("setting {}", s.id));
            let ok = !self.fail.contains(&s.id);
            done(SettingStepResult {
                step: s.clone(),
                ok,
                detail: (!ok).then(|| "boom".to_string()),
                dur_ms: 1,
            });
        }
        Ok(())
    }

    fn settled(&self, _req: &SettingsRequest<'_>) -> Vec<(String, String)> {
        self.settled.lock().unwrap().clone()
    }

    fn restart(&self, _req: &SettingsRequest<'_>, apps: &[String]) -> Vec<RestartResult> {
        self.log
            .lock()
            .unwrap()
            .push(format!("restart {}", apps.join(",")));
        apps.iter()
            .map(|a| RestartResult {
                app: a.clone(),
                ok: true,
                detail: None,
            })
            .collect()
    }
}

struct FakeServices {
    steps: Vec<ServiceStep>,
    log: Arc<Mutex<Vec<String>>>,
    changed_seen: Mutex<Vec<Vec<String>>>,
}

impl FakeServices {
    fn new(steps: Vec<ServiceStep>, log: Arc<Mutex<Vec<String>>>) -> Self {
        FakeServices {
            steps,
            log,
            changed_seen: Mutex::default(),
        }
    }
}

impl ServicesPhase for FakeServices {
    fn plan(&self, req: &ServicesRequest<'_>) -> Result<Vec<ServiceStep>, PlanError> {
        self.changed_seen.lock().unwrap().push(req.changed.to_vec());
        Ok(self
            .steps
            .iter()
            .filter(|s| req.packages.contains(&s.package))
            .cloned()
            .collect())
    }

    fn apply(
        &self,
        req: &ServicesRequest<'_>,
        steps: &[ServiceStep],
        done: &mut dyn FnMut(ServiceStepResult),
    ) -> Result<(), String> {
        for s in steps {
            assert_eq!(
                s.teardown, req.teardown,
                "teardown steps run in their own call"
            );
            self.log
                .lock()
                .unwrap()
                .push(format!("service {} {}", s.name, s.action));
            done(ServiceStepResult {
                step: s.clone(),
                ok: true,
                detail: None,
                dur_ms: 1,
            });
        }
        Ok(())
    }
}

/// A files phase logging into the shared journal.
struct LogFiles {
    steps: Vec<FileStep>,
    log: Arc<Mutex<Vec<String>>>,
    fail: Vec<String>,
}

impl FilesPhase for LogFiles {
    fn plan(&self, req: &FilesRequest<'_>) -> Result<Vec<FileStep>, String> {
        Ok(self
            .steps
            .iter()
            .filter(|s| req.packages.contains(&s.package))
            .cloned()
            .collect())
    }

    fn apply(
        &self,
        _req: &FilesRequest<'_>,
        steps: &[FileStep],
        done: &mut dyn FnMut(FileStepResult),
    ) -> Result<(), String> {
        for s in steps {
            self.log.lock().unwrap().push(format!("file {}", s.dst));
            let ok = !self.fail.contains(&s.dst);
            done(FileStepResult {
                step: s.clone(),
                ok,
                detail: (!ok).then(|| "denied".to_string()),
                dur_ms: 1,
            });
        }
        Ok(())
    }
}

#[test]
fn settings_and_services_run_after_files_and_before_nothing_else() {
    let sb = sandbox(&[("fd", BREW)], &[]);
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&brew]);
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let files = LogFiles {
        steps: vec![fstep("fd", "~/.fdrc", false)],
        log: log.clone(),
        fail: vec![],
    };
    let settings = FakeSettings::new(
        vec![sstep("fd", "defaults:d:k", &[], &[], false)],
        log.clone(),
    );
    let services = FakeServices::new(vec![vstep("fd", "fd", "start")], log.clone());
    let mut case = Case::new(&sb, &reg, &files);
    case.settings = &settings;
    case.services = &services;

    let plan = case.plan();
    assert_eq!(plan.settings.len(), 1);
    assert_eq!(plan.services.len(), 1);
    // the service is told which packages' config changes
    assert_eq!(services.changed_seen.lock().unwrap()[0], ["fd"]);
    assert!(!plan.is_empty() && plan.touches_more_than_owned_files());

    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    // the service starts after its config is placed, and after settings
    assert_eq!(
        *log.lock().unwrap(),
        ["file ~/.fdrc", "setting defaults:d:k", "service fd start"]
    );
    assert_eq!(brew.executed().len(), 1);
    assert_eq!((r.settings.len(), r.services.len()), (1, 1));
    // journaled
    let ops: Vec<String> = read_journal(&sb)
        .iter()
        .map(|e| e["op"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ops.iter().any(|o| o == "setting-set") && ops.iter().any(|o| o == "service-start"),
        "{ops:?}"
    );
    // json and text views carry both
    let j = plan_json(&plan);
    assert_eq!(j["settings"][0]["id"], "defaults:d:k");
    assert_eq!(j["services"][0]["commands"][0], "systemctl --user start fd");
    assert_eq!(j["summary"]["settings"], 1);
    let t = plan_text(&plan);
    assert!(
        t.contains("Settings (1)") && t.contains("Services (1)"),
        "{t}"
    );
    let aj = r.to_json();
    assert_eq!(aj["summary"]["settings_ok"], 1);
    assert_eq!(aj["summary"]["services_ok"], 1);
}

#[test]
fn restarts_are_collected_across_packages_and_run_once_at_the_end() {
    let sb = sandbox(&[("a", BREW), ("b", BREW), ("c", BREW)], &[]);
    let brew = installed_brew(&["a", "b", "c"]);
    let reg = registry(&[&brew]);
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let settings = FakeSettings::new(
        vec![
            sstep("a", "defaults:x:1", &["Dock"], &[], false),
            sstep(
                "b",
                "defaults:x:2",
                &["Dock"],
                &["restart-app:Finder"],
                false,
            ),
            sstep("c", "defaults:x:3", &["Dock"], &[], false),
        ],
        log.clone(),
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &settings;
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log.iter().filter(|l| l.starts_with("restart")).count(),
        1,
        "{log:?}"
    );
    // after the last change
    assert_eq!(log.last().unwrap(), "restart Dock,Finder");
    assert_eq!(r.restarts.len(), 2);
}

#[test]
fn a_logout_requirement_becomes_a_todo_in_the_summary_and_the_state() {
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let settings = FakeSettings::new(
        vec![sstep("a", "defaults:x:1", &[], &["logout"], false)],
        log,
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &settings;
    let r = case.apply(true);
    assert_eq!(r.todos.len(), 1);
    assert!(r.todos[0].reason.starts_with("logout:"), "{:?}", r.todos);
    assert_eq!(read_state(&sb)["todos"][0]["package"], "a");
    assert!(
        r.summary_text().contains("todo     a: logout:"),
        "{}",
        r.summary_text()
    );
    // not raised again by the same item
    let r2 = case.apply(true);
    assert_eq!(read_state(&sb)["todos"].as_array().unwrap().len(), 1);
    let _ = r2;
}

#[test]
fn root_settings_share_the_one_sudo_prompt_with_an_apt_install() {
    let sb = sandbox(&[("a", "[providers.apt]")], &[]);
    let apt = FakeProvider::new("apt").with_caps(apt_caps());
    let reg = registry(&[&apt]);
    let rec = Arc::new(RecordingRunner::new());
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let settings = FakeSettings::new(vec![sstep("a", "defaults:x:1", &[], &[], true)], log);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.ctx.runner = rec.clone();
    case.settings = &settings;
    assert!(case.plan().needs_root());
    assert!(
        plan_json(&case.plan())["summary"]["root_actions"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let mut c = cfg(true);
    c.sudo = true;
    let r = case.apply_with(&c, &NoPrompt);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    let primes = rec
        .lines()
        .iter()
        .filter(|l| *l == "/usr/bin/sudo -v")
        .count();
    assert_eq!(primes, 1);
    assert_eq!(*settings.sudo_seen.lock().unwrap(), [true]);

    // a plan that only needs root for a setting still primes sudo
    let sb2 = sandbox(&[("b", BREW)], &[]);
    let brew2 = installed_brew(&["b"]);
    let reg2 = registry(&[&brew2]);
    let rec2 = Arc::new(RecordingRunner::new());
    let s2 = FakeSettings::new(
        vec![sstep("b", "defaults:x:1", &[], &[], true)],
        Arc::default(),
    );
    let mut case2 = Case::new(&sb2, &reg2, &NoFiles);
    case2.ctx.runner = rec2.clone();
    case2.settings = &s2;
    let mut c2 = cfg(true);
    c2.sudo = true;
    case2.apply_with(&c2, &NoPrompt);
    assert_eq!(
        rec2.lines()
            .iter()
            .filter(|l| *l == "/usr/bin/sudo -v")
            .count(),
        1
    );
}

#[test]
fn settings_and_services_of_a_failed_install_are_left_alone() {
    let sb = sandbox(&[("fd", BREW), ("ok", BREW)], &[]);
    let brew = installed_brew(&["ok"]).failing_on("fd");
    let reg = registry(&[&brew]);
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let settings = FakeSettings::new(
        vec![
            sstep("fd", "defaults:x:fd", &[], &[], false),
            sstep("ok", "defaults:x:ok", &[], &[], false),
        ],
        log.clone(),
    );
    let services = FakeServices::new(
        vec![vstep("fd", "fd", "start"), vstep("ok", "ok", "start")],
        log.clone(),
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &settings;
    case.services = &services;
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(r.settings_skipped.len(), 1);
    assert_eq!(r.services_skipped.len(), 1);
    assert_eq!(
        *log.lock().unwrap(),
        ["setting defaults:x:ok", "service ok start"]
    );
}

#[test]
fn a_service_whose_config_failed_does_not_start() {
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let files = LogFiles {
        steps: vec![fstep("a", "~/.arc", false)],
        log: log.clone(),
        fail: vec!["~/.arc".into()],
    };
    let services = FakeServices::new(vec![vstep("a", "a", "start")], log.clone());
    let mut case = Case::new(&sb, &reg, &files);
    case.services = &services;
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(*log.lock().unwrap(), ["file ~/.arc"]);
    assert_eq!(r.services_skipped.len(), 1);
}

#[test]
fn a_failed_setting_fails_the_run_with_a_setting_error() {
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let mut settings = FakeSettings::new(
        vec![sstep("a", "defaults:x:1", &["Dock"], &["logout"], false)],
        Arc::default(),
    );
    settings.fail = vec!["defaults:x:1".into()];
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &settings;
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert!(r.errors.iter().any(|e| e.code == "setting-failed"));
    // nothing changed, so nothing restarts and nothing is asked of the user
    assert!(r.restarts.is_empty() && r.todos.is_empty());
}

#[test]
fn scoped_runs_skip_settings_and_services() {
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let settings = FakeSettings::new(
        vec![sstep("a", "defaults:x:1", &[], &[], false)],
        Arc::default(),
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &settings;
    case.scope.files_only = true;
    assert!(case.plan().settings.is_empty());
    case.scope.files_only = false;
    case.scope.packages_only = true;
    assert!(case.plan().settings.is_empty());
}

// ------------------------------------------------------------------ task 5.9: lock vs spec

fn limited_caps() -> Caps {
    use dots_providers::Support;
    Caps {
        exact_version: Support::Limited,
        ..FakeProvider::new("x").caps()
    }
}

fn lock_row(name: &str, provider: &str, version: &str) -> dots_lock::PkgRow {
    let key = "debian/x86_64";
    let mut r = dots_lock::PkgRow::new(name, &dots_lock::platform_for(provider, key), provider);
    r.version = Some(version.into());
    r.locked_at = "2026-11-25T20:11:03Z".into();
    r
}

fn own(sb: &Sandbox, name: &str, provider: &str, version: &str) {
    let mut st = State::default();
    st.packages.insert(
        name.into(),
        OwnedPackage {
            provider: provider.into(),
            name: name.into(),
            version: Some(version.into()),
            run: "r-0".into(),
            ts: "t".into(),
        },
    );
    st.save(&sb.dirs.state_json()).unwrap();
}

#[test]
fn the_locked_version_wins_under_latest() {
    let sb = sandbox(&[("fd", "[providers.brew]")], &[]);
    let brew = FakeProvider::new("brew").with_latest("fd", "2.0.0");
    let reg = registry(&[&brew]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("fd", "brew", "1.0.0"));
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    let op = &brew.executed()[0].ops[0];
    assert_eq!(
        op.version.as_deref(),
        Some("1.0.0"),
        "machines stay reproducible"
    );
    // The row already says so: nothing to write.
    assert!(r.lock_updates.is_empty(), "{:?}", r.lock_updates);
}

#[test]
fn a_first_install_is_recorded_for_the_lock() {
    let sb = sandbox(&[("fd", "[providers.brew]")], &[]);
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.lock_updates.len(), 1);
    let u = &r.lock_updates[0];
    assert_eq!((u.package.as_str(), u.provider.as_str()), ("fd", "brew"));
    assert_eq!(u.platform, "debian/x86_64", "brew rows are per platform");
}

#[test]
fn a_tightened_spec_moves_the_lock_row_to_the_newest_match() {
    let sb = sandbox(
        &[("bottom", "version = \">=0.10\"\n[providers.cargo]")],
        &[],
    );
    let cargo = FakeProvider::new("cargo")
        .with_installed("bottom", "0.9.6")
        .with_latest("bottom", "0.10.2");
    let reg = registry(&[&cargo]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("bottom", "cargo", "0.9.6"));
    case.offers
        .0
        .insert(("cargo".into(), "bottom".into()), "0.10.2".into());
    let p = case.plan();
    assert_eq!(p.steps[0].kind, StepKind::Upgrade);
    assert_eq!(p.steps[0].to.as_deref(), Some("0.10.2"));
    let l = &p.lock[0];
    assert_eq!(l.from.as_deref(), Some("0.9.6"));
    assert_eq!(l.to.as_deref(), Some("0.10.2"));
    assert!(l.rewrite);
    let text = plan_text(&p);
    assert!(
        text.contains("Lock") && text.contains("0.9.6 -> 0.10.2"),
        "{text}"
    );
    assert_eq!(plan_json(&p)["lock"][0]["to"], "0.10.2");
}

#[test]
fn a_limited_provider_installs_what_it_offers_and_reports_drift() {
    let sb = sandbox(&[("less", "[providers.apt]")], &[]);
    let apt = FakeProvider::new("apt")
        .with_caps(limited_caps())
        .with_latest("less", "8.7.1");
    let reg = registry(&[&apt]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("less", "apt", "8.7.0"));
    case.offers
        .0
        .insert(("apt".into(), "less".into()), "8.7.1".into());
    let p = case.plan();
    assert_eq!(p.steps[0].kind, StepKind::Install);
    assert_eq!(p.steps[0].to, None, "no exact version is asked of apt");
    assert_eq!(
        p.lock[0].drift.as_deref(),
        Some("locked 8.7.0, provider offers 8.7.1")
    );
    assert!(!p.lock[0].rewrite);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert!(apt.executed()[0].ops[0].version.is_none());
    assert!(r.lock_updates.is_empty(), "the lock keeps saying 8.7.0");
}

#[test]
fn a_lock_older_than_what_is_installed_downgrades_only_with_permission() {
    let sb = sandbox(&[("fd", "[providers.brew]")], &[]);
    own(&sb, "fd", "brew", "2.0.0");
    let brew = FakeProvider::new("brew").with_installed("fd", "2.0.0");
    let reg = registry(&[&brew]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("fd", "brew", "1.0.0"));
    let p = case.plan();
    assert_eq!(p.steps[0].kind, StepKind::Downgrade);
    assert!(
        plan_text(&p).contains("\u{26a0} downgrade fd"),
        "{}",
        plan_text(&p)
    );
    // `--yes --no-input` is refused and says what to pass.
    let r = case.apply(true);
    assert_eq!(r.status, Status::ConfirmationRequired);
    assert!(r.errors[0].hint.contains("--allow-downgrade"));
    assert!(brew.executed().is_empty());
    let mut c = cfg(true);
    c.allow_downgrade = true;
    assert_eq!(case.apply_with(&c, &NoPrompt).status, Status::Done);
    assert_eq!(brew.executed()[0].ops[0].version.as_deref(), Some("1.0.0"));
}

#[test]
fn a_newer_install_under_a_limited_provider_is_left_alone() {
    let sb = sandbox(&[("less", "[providers.apt]")], &[]);
    own(&sb, "less", "apt", "8.7.1");
    let apt = FakeProvider::new("apt")
        .with_caps(limited_caps())
        .with_installed("less", "8.7.1");
    let reg = registry(&[&apt]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("less", "apt", "8.7.0"));
    assert!(
        case.plan().steps.is_empty(),
        "apt cannot go down; status reports drift"
    );
}

#[test]
fn a_version_installed_outside_dots_is_adopted_not_reinstalled() {
    let sb = sandbox(&[("fd", "version = \"1.2\"\n[providers.brew]")], &[]);
    let brew = FakeProvider::new("brew").with_installed("fd", "1.2.9");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let p = case.plan();
    assert!(p.steps.is_empty());
    assert!(p.lock[0].adopt);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert!(brew.executed().is_empty());
    assert_eq!(r.lock_updates[0].version.as_deref(), Some("1.2.9"));
    assert!(r.lock_updates[0].adopt);
    assert_eq!(read_journal(&sb)[0]["op"], "adopt");
}

#[test]
fn upgrade_targets_override_the_lock() {
    let sb = sandbox(&[("fd", "version = \"0.11\"\n[providers.cargo]")], &[]);
    own(&sb, "fd", "cargo", "0.11.2");
    let cargo = FakeProvider::new("cargo").with_installed("fd", "0.11.2");
    let reg = registry(&[&cargo]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("fd", "cargo", "0.11.2"));
    case.upgrade.insert(
        "fd".into(),
        UpgradeTarget {
            version: Some("0.11.9".into()),
            to: Some("0.11.9".into()),
        },
    );
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert_eq!(r.steps[0].action, "upgrade");
    assert_eq!(
        cargo.executed()[0].ops[0].version.as_deref(),
        Some("0.11.9")
    );
    assert_eq!(read_journal(&sb)[0]["op"], "upgrade");
    assert_eq!(r.lock_updates.len(), 1);
}

// ------------------------------------------------------------------ review follow-ups

/// A provider recording the `wanted` set `plan_with_wanted` is given.
struct WantedProbe {
    inner: FakeProvider,
    wanted: Arc<Mutex<Vec<Vec<String>>>>,
    plain_plan_calls: Arc<Mutex<usize>>,
}

impl Provider for WantedProbe {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn caps(&self) -> Caps {
        self.inner.caps()
    }
    fn availability(&self, f: &Facts) -> Availability {
        self.inner.availability(f)
    }
    fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
        self.inner.installed(c)
    }
    fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
        self.inner.latest(c, s)
    }
    fn plan(&self, o: &[Op]) -> PResult<Vec<Action>> {
        *self.plain_plan_calls.lock().unwrap() += 1;
        self.inner.plan(o)
    }
    fn plan_with_wanted(&self, o: &[Op], wanted: &[Spec]) -> PResult<Vec<Action>> {
        let mut names: Vec<String> = wanted.iter().map(|s| s.name.clone()).collect();
        names.sort();
        self.wanted.lock().unwrap().push(names);
        self.inner.plan(o)
    }
    fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
        self.inner.exec(c, a)
    }
    fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
        self.inner.bins(c, i)
    }
}

#[test]
fn batch_planning_passes_the_whole_desired_set_to_the_provider() {
    // `a` needs an install; `b` and `c` are installed already (no step), but all three stay
    // wanted, so a shared resource of theirs (an apt repository) must be kept.
    let sb = sandbox(&[("a", BREW), ("b", BREW), ("c", BREW)], &[]);
    let wanted = Arc::new(Mutex::new(Vec::new()));
    let plain = Arc::new(Mutex::new(0usize));
    let mut reg = Registry::new();
    reg.register(Box::new(WantedProbe {
        inner: installed_brew(&["b", "c"]),
        wanted: wanted.clone(),
        plain_plan_calls: plain.clone(),
    }));
    let case = Case::new(&sb, &reg, &NoFiles);
    let p = case.plan();
    assert_eq!(p.steps.len(), 1);
    assert_eq!(*wanted.lock().unwrap(), [["a", "b", "c"]]);
    assert_eq!(
        *plain.lock().unwrap(),
        0,
        "the engine must not call plain `plan`"
    );
}

#[test]
fn closing_settled_todos_happens_before_settings_apply_and_in_empty_plans() {
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let reason = dots_settings::todo_reason("logout", "defaults:x:1");
    let settings = FakeSettings::new(
        vec![sstep("a", "defaults:x:1", &[], &["logout"], false)],
        Arc::default(),
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &settings;
    // the run raises the todo
    case.apply(true);
    assert_eq!(read_state(&sb)["todos"][0]["reason"], reason.as_str());
    // the user logged out: the setting reads as declared, so the todo closes
    *settings.settled.lock().unwrap() = vec![("a".into(), reason.clone())];
    let none = FakeSettings::new(vec![], Arc::default());
    *none.settled.lock().unwrap() = vec![("a".into(), reason.clone())];
    case.settings = &none;
    let r = case.apply(true); // nothing to apply at all: the empty-plan path
    assert_eq!(r.status, Status::Done);
    assert!(
        read_state(&sb)["todos"].as_array().unwrap().is_empty(),
        "{}",
        read_state(&sb)
    );

    // and in a run that applies settings: closed first, raised again only if still needed
    let sb2 = sandbox(&[("a", BREW)], &[]);
    let mut case2 = Case::new(&sb2, &reg, &NoFiles);
    let s2 = FakeSettings::new(
        vec![sstep("a", "defaults:x:2", &[], &[], false)],
        Arc::default(),
    );
    let mut st = State::default();
    st.todos.push(Todo {
        package: "a".into(),
        reason: reason.clone(),
        run: "r-old".into(),
    });
    st.todos.push(Todo {
        package: "a".into(),
        reason: "other".into(),
        run: "r-old".into(),
    });
    st.save(&sb2.dirs.state_json()).unwrap();
    *s2.settled.lock().unwrap() = vec![("a".into(), reason)];
    case2.settings = &s2;
    case2.apply(true);
    let todos = read_state(&sb2)["todos"].clone();
    assert_eq!(todos.as_array().unwrap().len(), 1, "{todos}");
    assert_eq!(todos[0]["reason"], "other");
}

#[test]
fn service_teardown_runs_before_files_are_removed() {
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let files = LogFiles {
        steps: vec![fstep("a", "~/.config/systemd/user/gone.service", true)],
        log: log.clone(),
        fail: vec![],
    };
    let mut gone = vstep("a", "gone", "disable");
    gone.teardown = true;
    let services = FakeServices::new(vec![gone, vstep("a", "kept", "restart")], log.clone());
    let mut case = Case::new(&sb, &reg, &files);
    case.services = &services;
    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    let log = log.lock().unwrap().clone();
    assert_eq!(
        log,
        [
            "service gone disable",
            "file ~/.config/systemd/user/gone.service",
            "service kept restart"
        ]
    );
    assert_eq!(r.services.len(), 2);
}

#[test]
fn a_phase_plan_error_keeps_its_own_code() {
    struct BadNames;
    impl ServicesPhase for BadNames {
        fn plan(&self, _: &ServicesRequest<'_>) -> Result<Vec<ServiceStep>, PlanError> {
            Err(dots_core::Diagnostic::from_code(
                "service-name-invalid",
                &[("name", "-x"), ("package", "a")],
            )
            .into())
        }
    }
    struct PlainBad;
    impl SettingsPhase for PlainBad {
        fn plan(&self, _: &SettingsRequest<'_>) -> Result<Vec<SettingStep>, PlanError> {
            Err("could not read".to_string().into())
        }
    }
    let sb = sandbox(&[("a", BREW)], &[]);
    let brew = installed_brew(&["a"]);
    let reg = registry(&[&brew]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.services = &BadNames;
    let r = case.apply(true);
    assert_eq!(r.status, Status::PlanFailed);
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].code, "service-name-invalid");
    // a plain message is still wrapped
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.settings = &PlainBad;
    let r = case.apply(true);
    assert_eq!(r.errors[0].code, "plan-failed");
    assert!(r.errors[0].message.contains("could not read"));
}

#[test]
fn a_failed_sudo_prime_tells_the_settings_phase_there_is_no_sudo() {
    // The phase still runs (it fails root entries itself, with a clear error); what matters is
    // that it is told `sudo: false`, so it never runs a root write as the user.
    let sb = sandbox(&[("b", BREW)], &[]);
    let brew = installed_brew(&["b"]);
    let reg = registry(&[&brew]);
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &["/usr/bin/sudo", "-v"],
        dots_providers::Output::fail(1, "no tty"),
    );
    let settings = FakeSettings::new(
        vec![sstep("b", "defaults:x:1", &[], &[], true)],
        Arc::default(),
    );
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.ctx.runner = rec.clone();
    case.settings = &settings;
    let mut c = cfg(true);
    c.sudo = true;
    case.apply_with(&c, &NoPrompt);
    assert_eq!(*settings.sudo_seen.lock().unwrap(), [false]);
    // --no-sudo likewise
    let settings2 = FakeSettings::new(
        vec![sstep("b", "defaults:x:1", &[], &[], true)],
        Arc::default(),
    );
    let mut case2 = Case::new(&sb, &reg, &NoFiles);
    case2.settings = &settings2;
    case2.apply(true);
    assert_eq!(*settings2.sudo_seen.lock().unwrap(), [false]);
}

/// Mimics apt: a package from a repository gets `add` + `install` actions; when the repo
/// cannot be prepared both report failure (the install is skipped, `success: false`).
struct RepoFails {
    inner: FakeProvider,
}

impl Provider for RepoFails {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn caps(&self) -> Caps {
        self.inner.caps()
    }
    fn availability(&self, f: &Facts) -> Availability {
        self.inner.availability(f)
    }
    fn installed(&self, c: &Ctx) -> PResult<BTreeMap<String, Installed>> {
        self.inner.installed(c)
    }
    fn latest(&self, c: &Ctx, s: &[Spec]) -> PResult<BTreeMap<String, Latest>> {
        self.inner.latest(c, s)
    }
    fn plan(&self, ops: &[Op]) -> PResult<Vec<Action>> {
        let (repo, plain): (Vec<Op>, Vec<Op>) = ops
            .iter()
            .cloned()
            .partition(|o| o.spec.name.starts_with("ppa-"));
        let mut actions = self.inner.plan(&plain)?;
        for op in repo {
            for verb in ["repo-add", "install-skipped"] {
                actions.push(Action {
                    kind: op.kind,
                    ops: vec![op.clone()],
                    argv: vec![verb.into(), op.spec.name.clone()],
                    needs_root: false,
                });
            }
        }
        Ok(actions)
    }
    fn exec(&self, c: &Ctx, a: &Action) -> PResult<Outcome> {
        if a.argv
            .first()
            .is_some_and(|v| v == "repo-add" || v == "install-skipped")
        {
            return Ok(Outcome {
                success: false,
                log: None,
            });
        }
        self.inner.exec(c, a)
    }
    fn bins(&self, c: &Ctx, i: &BTreeMap<String, Installed>) -> PResult<Vec<BinEntry>> {
        self.inner.bins(c, i)
    }
}

#[test]
fn packages_behind_a_failed_repo_are_failed_not_installed() {
    let sb = sandbox(
        &[
            ("ppa-tool", BREW),
            ("plain", BREW),
            ("after", "requires = [\"ppa-tool\"]\n[providers.brew]"),
        ],
        &[],
    );
    let mut reg = Registry::new();
    reg.register(Box::new(RepoFails {
        inner: FakeProvider::new("brew"),
    }));
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert_eq!(status_of(&r, "ppa-tool"), &StepStatus::Failed);
    assert_eq!(status_of(&r, "plain"), &StepStatus::Ok);
    // its dependent is skipped, not failed
    assert!(
        matches!(status_of(&r, "after"), StepStatus::SkippedDueToDep { .. }),
        "{:?}",
        status_of(&r, "after")
    );
    let st = read_state(&sb);
    assert!(st["packages"].get("ppa-tool").is_none(), "{st}");
    assert!(st["packages"].get("after").is_none(), "{st}");
    assert!(st["packages"].get("plain").is_some(), "{st}");
}

// ------------------------------------------------------------------ hooks

mod hooks {
    use super::*;

    /// A package `x` on brew with the given `[hooks]` lines; each hook appends its name to
    /// `<tmp>/order.txt`.
    fn hooked(hooks: &[(&str, &str)]) -> (Sandbox, PathBuf) {
        let sb = sandbox(&[("x", "[providers.brew]")], &[]);
        let order = sb.root.parent().unwrap().join("order.txt");
        let mut body = String::from("[providers.brew]\n[hooks]\n");
        for (event, cmd) in hooks {
            body.push_str(&format!(
                "{event} = \"{}\"\n",
                cmd.replace("{order}", &order.display().to_string())
            ));
        }
        write(
            &sb.root.join("packages/x/package.toml"),
            &format!("schema = 1\nname = \"x\"\n{body}"),
        );
        (sb, order)
    }

    fn files_for_x() -> FakeFiles {
        FakeFiles::new(vec![fstep("x", "~/.xrc", false)])
    }

    fn lines(p: &Path) -> Vec<String> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    const ALL: &[(&str, &str)] = &[
        ("pre_install", "echo pre_install >> {order}"),
        ("post_install", "echo post_install >> {order}"),
        ("pre_files", "echo pre_files >> {order}"),
        ("post_files", "echo post_files >> {order}"),
    ];

    #[test]
    fn all_four_hooks_run_in_pipeline_order_and_are_logged_and_journalled() {
        let (sb, order) = hooked(ALL);
        let reg = registry(&[&FakeProvider::new("brew")]);
        let files = files_for_x();
        let case = Case::new(&sb, &reg, &files);
        let r = case.apply(true);
        assert_eq!(r.status, Status::Done, "{:?}", r.errors);
        assert_eq!(
            lines(&order),
            ["pre_install", "pre_files", "post_install", "post_files"]
        );
        assert_eq!(r.hooks.len(), 4);
        assert!(r.hooks.iter().all(|h| h.ok));
        let log = std::fs::read_to_string(sb.dirs.logs("r-test").join("x.hook-post_files.log"))
            .expect("hook log");
        assert!(
            log.contains("echo post_files") && log.contains("exit status 0"),
            "{log}"
        );
        let ops: Vec<String> = read_journal(&sb)
            .iter()
            .filter_map(|e| e["op"].as_str().map(String::from))
            .filter(|o| o.starts_with("hook-"))
            .collect();
        assert_eq!(
            ops,
            [
                "hook-pre_install",
                "hook-pre_files",
                "hook-post_install",
                "hook-post_files"
            ]
        );
    }

    #[test]
    fn plan_lists_the_hooks_and_dry_run_runs_none() {
        let (sb, order) = hooked(ALL);
        let reg = registry(&[&FakeProvider::new("brew")]);
        let files = files_for_x();
        let case = Case::new(&sb, &reg, &files);
        let plan = case.plan();
        let events: Vec<&str> = plan.hooks.iter().map(|h| h.event.as_str()).collect();
        assert_eq!(
            events,
            ["pre_install", "post_install", "pre_files", "post_files"]
        );
        assert!(
            plan_text(&plan).contains("Hooks (4)"),
            "{}",
            plan_text(&plan)
        );
        assert_eq!(plan_json(&plan)["hooks"].as_array().unwrap().len(), 4);
        let lock = RunLock::acquire(&sb.dirs.run_lock()).unwrap();
        drop(lock);
        let r = case.with(|i| apply(i, &cfg(true), &NoPrompt, Mode::DryRun));
        assert_eq!(r.status, Status::DryRun);
        assert_eq!(r.plan.hooks.len(), 4);
        assert!(r.hooks.is_empty());
        assert!(!order.exists(), "a dry run must not run hooks");
    }

    #[test]
    fn hooks_fire_only_when_their_trigger_changed() {
        let (sb, order) = hooked(ALL);
        // Already installed, no file changes: nothing fires.
        let brew = FakeProvider::new("brew").with_installed("x", "1.0.0");
        let reg = registry(&[&brew]);
        let case = Case::new(&sb, &reg, &NoFiles);
        assert!(case.plan().hooks.is_empty());
        assert_eq!(case.apply(true).status, Status::Done);
        assert!(!order.exists());
        // Installed, files change: only the files hooks.
        let files = files_for_x();
        let case = Case::new(&sb, &reg, &files);
        let events: Vec<String> = case.plan().hooks.into_iter().map(|h| h.event).collect();
        assert_eq!(events, ["pre_files", "post_files"]);
        // Not installed, no files: only the install hooks.
        let reg = registry(&[&FakeProvider::new("brew")]);
        let case = Case::new(&sb, &reg, &NoFiles);
        let events: Vec<String> = case.plan().hooks.into_iter().map(|h| h.event).collect();
        assert_eq!(events, ["pre_install", "post_install"]);
    }

    #[test]
    fn scope_selects_the_install_or_the_files_half() {
        let (sb, _) = hooked(ALL);
        let reg = registry(&[&FakeProvider::new("brew")]);
        let files = files_for_x();
        let mut case = Case::new(&sb, &reg, &files);
        case.scope.files_only = true;
        let events: Vec<String> = case.plan().hooks.into_iter().map(|h| h.event).collect();
        assert_eq!(events, ["pre_files", "post_files"]);
        case.scope = Scope {
            packages_only: true,
            ..Scope::default()
        };
        let events: Vec<String> = case.plan().hooks.into_iter().map(|h| h.event).collect();
        assert_eq!(events, ["pre_install", "post_install"]);
    }

    #[test]
    fn a_failing_post_hook_fails_the_run_skips_the_rest_and_is_retried() {
        let (sb, order) = hooked(&[
            ("post_install", "echo post_install >> {order}; exit 3"),
            ("post_files", "echo post_files >> {order}"),
        ]);
        let brew = FakeProvider::new("brew");
        let reg = registry(&[&brew]);
        let files = files_for_x();
        let case = Case::new(&sb, &reg, &files);
        let r = case.apply(true);
        assert_eq!(r.status, Status::Failed);
        assert_eq!(
            lines(&order),
            ["post_install"],
            "post_files must be skipped"
        );
        assert_eq!(r.hooks.len(), 1);
        assert!(!r.hooks[0].ok);
        assert_eq!(r.hooks_skipped.len(), 1);
        assert_eq!(r.hooks_skipped[0].0.event, "post_files");
        let err = r
            .errors
            .iter()
            .find(|e| e.code == "hook-failed")
            .expect("hook-failed");
        assert!(
            err.message.contains("post_install") && err.message.contains("x.hook-post_install.log")
        );
        // The package itself installed and its files landed.
        assert_eq!(status_of(&r, "x"), &StepStatus::Ok);
        assert_eq!(r.files.len(), 1);
        let entry = read_journal(&sb)
            .into_iter()
            .find(|e| e["op"] == "hook-post_install")
            .unwrap();
        assert_eq!(entry["exit"], 1);
        assert_eq!(read_state(&sb)["hooks_failed"][0], "x:post_install");
        // Next run: the package is installed and nothing changes, but the failed hook runs again.
        let reg2 = registry(&[&FakeProvider::new("brew").with_installed("x", "1.0.0")]);
        let case = Case::new(&sb, &reg2, &NoFiles);
        let plan = case.plan();
        assert_eq!(plan.hooks.len(), 1);
        assert_eq!(plan.hooks[0].why, "failed last run");
        write(
            &sb.root.join("packages/x/package.toml"),
            "schema = 1\nname = \"x\"\n[providers.brew]\n[hooks]\npost_install = \"true\"\n",
        );
        let r = case.apply(true);
        assert_eq!(r.status, Status::Done, "{:?}", r.errors);
        assert!(read_state(&sb).get("hooks_failed").is_none());
    }

    #[test]
    fn a_failing_pre_install_hook_stops_the_install() {
        let (sb, _) = hooked(&[("pre_install", "exit 7"), ("post_install", "true")]);
        let brew = FakeProvider::new("brew");
        let reg = registry(&[&brew]);
        let files = files_for_x();
        let case = Case::new(&sb, &reg, &files);
        let r = case.apply(true);
        assert_eq!(r.status, Status::Failed);
        assert!(brew.executed().is_empty(), "the install must not run");
        assert_eq!(status_of(&r, "x"), &StepStatus::Failed);
        assert!(
            r.files.is_empty(),
            "files of a package that did not install are left alone"
        );
        assert_eq!(r.hooks_skipped.len(), 1, "post_install is skipped");
        assert!(r.errors.iter().any(|e| e.code == "hook-failed"));
    }

    #[test]
    fn a_failing_pre_files_hook_leaves_the_files_alone() {
        let (sb, _) = hooked(&[("pre_files", "exit 1"), ("post_files", "true")]);
        let reg = registry(&[&FakeProvider::new("brew").with_installed("x", "1")]);
        let files = files_for_x();
        let case = Case::new(&sb, &reg, &files);
        let r = case.apply(true);
        assert_eq!(r.status, Status::Failed);
        assert!(r.files.is_empty());
        assert_eq!(r.files_skipped.len(), 1);
        assert_eq!(r.hooks_skipped.len(), 1);
    }

    #[test]
    fn hooks_ask_for_confirmation_because_they_run_code() {
        let (sb, _) = hooked(&[("post_files", "true")]);
        let reg = registry(&[&FakeProvider::new("brew").with_installed("x", "1")]);
        let files = FakeFiles::new(vec![fstep("x", "~/.xrc", true)]);
        let case = Case::new(&sb, &reg, &files);
        // Only owned files would need no question; the hook does.
        assert!(case.plan().touches_more_than_owned_files());
        let r = case.apply_with(&cfg(false), &NoPrompt);
        assert_eq!(r.status, Status::ConfirmationRequired);
    }
}

// ------------------------------------------------------------------ shims

mod shims {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A repo with `fd` (brew, `bin = "fdfind"`, `shim = "fd"`), a PATH dir holding an
    /// executable `fdfind`, and a context whose PATH is that dir.
    fn fixture() -> (Sandbox, PathBuf, PathBuf) {
        let sb = sandbox(
            &[("fd", "[providers.brew]\nbin = \"fdfind\"\nshim = \"fd\"")],
            &[],
        );
        let bin = sb.root.parent().unwrap().join("pathdir");
        write(&bin.join("fdfind"), "#!/bin/sh\n");
        std::fs::set_permissions(bin.join("fdfind"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let link = sb.dirs.home.join(".local/bin/fd");
        (sb, bin, link)
    }

    fn with_path<'a>(case: &mut Case<'a>, dir: &Path) {
        case.ctx
            .env
            .insert("PATH".into(), dir.to_string_lossy().into_owned());
    }

    #[test]
    fn install_creates_the_shim_records_it_and_a_second_run_leaves_it() {
        let (sb, bin, link) = fixture();
        let reg = registry(&[&FakeProvider::new("brew")]);
        let mut case = Case::new(&sb, &reg, &NoFiles);
        with_path(&mut case, &bin);
        let plan = case.plan();
        assert_eq!(plan.shims.len(), 1);
        assert_eq!(plan.shims[0].dst, "~/.local/bin/fd");
        assert!(plan_text(&plan).contains("Shims (1)"));
        let r = case.apply(true);
        assert_eq!(r.status, Status::Done, "{:?}", r.errors);
        assert_eq!(r.shims.len(), 1);
        assert_eq!(std::fs::read_link(&link).unwrap(), bin.join("fdfind"));
        let state = read_state(&sb);
        assert_eq!(
            state["shims"][link.to_str().unwrap()]["package"],
            "fd",
            "{state}"
        );
        assert!(read_journal(&sb).iter().any(|e| e["op"] == "shim-create"));
        // Installed and linked: nothing to do.
        let reg = registry(&[&FakeProvider::new("brew").with_installed("fd", "1")]);
        let mut case = Case::new(&sb, &reg, &NoFiles);
        with_path(&mut case, &bin);
        assert!(case.plan().shims.is_empty());
    }

    #[test]
    fn a_dangling_shim_is_repaired_and_a_real_file_is_never_replaced() {
        let (sb, bin, link) = fixture();
        let reg = registry(&[&FakeProvider::new("brew").with_installed("fd", "1")]);
        let mut case = Case::new(&sb, &reg, &NoFiles);
        with_path(&mut case, &bin);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/nonexistent/fdfind", &link).unwrap();
        let plan = case.plan();
        assert_eq!(plan.shims[0].action, "update");
        assert_eq!(case.apply(true).status, Status::Done);
        assert_eq!(std::fs::read_link(&link).unwrap(), bin.join("fdfind"));
        // A real file in the way.
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "mine").unwrap();
        let r = case.apply(true);
        assert_eq!(r.status, Status::Failed);
        assert!(
            r.errors.iter().any(|e| e.message.contains("not a symlink")),
            "{:?}",
            r.errors
        );
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "mine");
    }

    #[test]
    fn a_missing_target_fails_the_shim_not_the_package() {
        let (sb, _bin, link) = fixture();
        let reg = registry(&[&FakeProvider::new("brew")]);
        let mut case = Case::new(&sb, &reg, &NoFiles);
        with_path(&mut case, &sb.root); // no `fdfind` here
        let r = case.apply(true);
        assert_eq!(r.status, Status::Failed);
        assert_eq!(status_of(&r, "fd"), &StepStatus::Ok);
        assert!(!r.shims[0].ok);
        assert!(r.shims[0].detail.as_deref().unwrap().contains("fdfind"));
        assert!(std::fs::symlink_metadata(&link).is_err());
    }

    #[test]
    fn a_shim_of_a_dropped_package_is_removed_on_a_full_run_only() {
        let (sb, bin, link) = fixture();
        // dots made `~/.local/bin/old` for a package that is no longer in the repo.
        let old = sb.dirs.home.join(".local/bin/old");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(bin.join("fdfind"), &old).unwrap();
        write(
            &sb.dirs.state_json(),
            &format!(
                r#"{{"schema":1,"shims":{{{:?}:{{"package":"old","target":"x","run":"r","ts":"t"}}}}}}"#,
                old.to_str().unwrap()
            ),
        );
        let reg = registry(&[&FakeProvider::new("brew").with_installed("fd", "1")]);
        let mut case = Case::new(&sb, &reg, &NoFiles);
        with_path(&mut case, &bin);
        // A run scoped to one package leaves it alone.
        case.scope.targets = vec!["fd".into()];
        assert!(case.plan().shims.iter().all(|s| s.action != "remove"));
        case.scope.targets.clear();
        let plan = case.plan();
        assert!(
            plan.shims
                .iter()
                .any(|s| s.action == "remove" && s.package == "old")
        );
        let r = case.apply(true);
        assert_eq!(r.status, Status::Done, "{:?}", r.errors);
        assert!(
            std::fs::symlink_metadata(&old).is_err(),
            "the dropped shim is gone"
        );
        assert!(
            read_state(&sb)
                .get("shims")
                .is_some_and(|s| s.get(old.to_str().unwrap()).is_none())
        );
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "fd's own shim was created"
        );
    }
}

// ------------------------------------------------------------------ provider switches

#[test]
fn a_provider_switch_installs_the_new_copy_then_removes_the_one_dots_installed() {
    // dots installed fzf with apt; the repo now says brew.
    let sb = sandbox(&[("fzf", "[providers.brew]")], &[]);
    own(&sb, "fzf", "apt", "0.44.1");
    let apt = FakeProvider::new("apt").with_installed("fzf", "0.44.1");
    let brew = FakeProvider::new("brew").with_latest("fzf", "0.60.0");
    let reg = registry(&[&apt, &brew]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("fzf", "apt", "0.44.1"));

    let p = case.plan();
    assert_eq!(p.steps.len(), 1, "{p:#?}");
    let sw = p.steps[0].switch.as_ref().expect("a switch");
    assert_eq!((sw.provider.as_str(), sw.native.as_str()), ("apt", "fzf"));
    assert_eq!(sw.version.as_deref(), Some("0.44.1"));
    assert!(sw.keep.is_none(), "{sw:?}");
    assert_eq!(sw.remove.len(), 1);
    assert_eq!(sw.remove[0].argv[..2], ["apt", "remove"]);
    let text = plan_text(&p);
    assert!(
        text.contains("apt -> brew") && text.contains("[then] apt remove fzf"),
        "{text}"
    );
    assert!(
        plan_json(&p)["steps"][0]["replaces"]["removes"] == true,
        "{}",
        plan_json(&p)
    );

    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert!(brew.snapshot_installed().contains_key("fzf"));
    assert!(
        !apt.snapshot_installed().contains_key("fzf"),
        "the old copy is gone"
    );
    assert_eq!(r.removals.len(), 1);
    assert!(r.removals[0].removed, "{:?}", r.removals);
    // State and lock follow the new provider.
    let (state, _) = State::load(&sb.dirs.state_json());
    assert_eq!(state.packages["fzf"].provider, "brew");
    assert!(
        r.lock_updates
            .iter()
            .any(|u| u.package == "fzf" && u.provider == "brew")
    );
    assert_eq!(
        r.lock_drops,
        vec![LockDrop {
            package: "fzf".into(),
            provider: "apt".into(),
            platform: "debian/x86_64".into()
        }]
    );
}

#[test]
fn a_failed_new_install_leaves_the_old_copy_alone() {
    let sb = sandbox(&[("fzf", "[providers.brew]")], &[]);
    own(&sb, "fzf", "apt", "0.44.1");
    let apt = FakeProvider::new("apt").with_installed("fzf", "0.44.1");
    let brew = FakeProvider::new("brew").failing_on("fzf");
    let reg = registry(&[&apt, &brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    let r = case.apply(true);
    assert_eq!(r.status, Status::Failed);
    assert!(
        apt.snapshot_installed().contains_key("fzf"),
        "never missing"
    );
    assert!(apt.executed().is_empty());
    assert!(r.removals.is_empty() && r.lock_drops.is_empty());
}

#[test]
fn a_switch_keeps_an_old_copy_dots_did_not_install() {
    // Only the lock knows the apt copy (adopted): no state.json record.
    let sb = sandbox(&[("fzf", "[providers.brew]")], &[]);
    let apt = FakeProvider::new("apt").with_installed("fzf", "0.44.1");
    let brew = FakeProvider::new("brew");
    let reg = registry(&[&apt, &brew]);
    let mut case = Case::new(&sb, &reg, &NoFiles);
    case.lock.upsert(lock_row("fzf", "apt", "0.44.1"));

    let p = case.plan();
    let sw = p.steps[0].switch.as_ref().expect("a switch");
    assert!(sw.remove.is_empty() && sw.keep.is_some(), "{sw:?}");
    assert!(plan_text(&p).contains("keeps the old apt copy"));

    let r = case.apply(true);
    assert_eq!(r.status, Status::Done, "{:#?}", r.errors);
    assert!(
        apt.snapshot_installed().contains_key("fzf"),
        "left in place"
    );
    assert!(brew.snapshot_installed().contains_key("fzf"));
    assert!(!r.removals[0].removed && r.removals[0].detail.is_some());
    // The lock row still moves to the new provider.
    assert_eq!(r.lock_drops.len(), 1);
}

#[test]
fn nothing_is_switched_when_the_same_provider_installed_it() {
    let sb = sandbox(&[("fzf", "[providers.brew]")], &[]);
    own(&sb, "fzf", "brew", "0.44.1");
    let brew = FakeProvider::new("brew").with_installed("fzf", "0.44.1");
    let reg = registry(&[&brew]);
    let case = Case::new(&sb, &reg, &NoFiles);
    assert!(case.plan().steps.iter().all(|s| s.switch.is_none()));
}
