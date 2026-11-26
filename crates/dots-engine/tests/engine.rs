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
use dots_resolve::{InstalledMap, resolve};
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
    scope: Scope,
    unreviewed: BTreeSet<String>,
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
            scope: Scope::default(),
            unreviewed: BTreeSet::new(),
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
        let res = resolve(&dots_resolve::Inputs {
            repo: &repo,
            machine: &machine,
            facts: &facts,
            chain: &chain,
            providers: self.reg,
            installed: &installed,
        });
        assert!(res.is_ok(), "{:#?}", res.errors);
        f(&Inputs {
            repo: &repo,
            resolution: &res,
            registry: self.reg,
            installed: &installed,
            files: self.files,
            ctx: &self.ctx,
            dirs: &self.sb.dirs,
            scope: &self.scope,
            unreviewed: &self.unreviewed,
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
