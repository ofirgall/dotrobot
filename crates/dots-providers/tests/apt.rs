//! apt provider tests: scripted conformance (fixtures captured from real Ubuntu containers),
//! parsing and plan checks, and real-container runs (see `tests/containers/apt/`).
//!
//! * default `cargo test` is hermetic: nothing here runs apt.
//! * `DOTS_CONTAINER_TESTS=1 cargo test -p dots-providers --test apt container_matrix` drives
//!   Docker: it runs the `#[ignore]`d `real_*` tests inside Ubuntu 22.04 and 24.04.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dots_facts::Facts;
use dots_providers::apt::AptProvider;
use dots_providers::{
    Action, Availability, Cmd, Ctx, Op, OpKind, Output, Provider, RealRunner, Result, Runner, Spec,
};
use dots_testkit::ConformanceFixture;

fn fixture(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/apt")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn op(kind: OpKind, name: &str, version: Option<&str>) -> Op {
    Op {
        kind,
        spec: Spec::new(name),
        version: version.map(str::to_string),
    }
}

fn ubuntu_facts(version: &str) -> Facts {
    Facts {
        os: "linux".into(),
        arch: "aarch64".into(),
        distro: Some("ubuntu".into()),
        distro_family: Some("debian".into()),
        distro_version: Some(version.into()),
        ..Facts::default()
    }
}

/// A tiny in-memory dpkg/apt: serves captured fixtures and applies installs.
#[derive(Debug)]
struct FakeApt {
    tag: &'static str,
    extra: Mutex<Vec<String>>,
}

impl FakeApt {
    fn new(tag: &'static str) -> Arc<Self> {
        Arc::new(FakeApt {
            tag,
            extra: Mutex::new(Vec::new()),
        })
    }
}

impl Runner for FakeApt {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let prog = cmd.program.to_string_lossy();
        let args = &cmd.args;
        match prog.as_ref() {
            "/usr/bin/dpkg-query" if args.first().map(String::as_str) == Some("-W") => {
                let mut out = fixture(&format!("dpkg-query.{}.txt", self.tag));
                for p in self.extra.lock().unwrap().iter() {
                    out.push_str(&format!("{p}\t1.0\tii \n"));
                }
                Ok(Output::ok(&out))
            }
            "/usr/bin/apt-cache" => Ok(Output::ok(&fixture(&format!(
                "apt-cache-policy.{}.txt",
                self.tag
            )))),
            "/usr/bin/env" => {
                let i = args.iter().position(|a| a == "install").unwrap();
                let pkgs = args[i + 1..].iter().filter(|a| !a.starts_with('-'));
                self.extra
                    .lock()
                    .unwrap()
                    .extend(pkgs.map(|p| p.split('=').next().unwrap().to_string()));
                Ok(Output::ok("done\n"))
            }
            _ => Ok(Output::fail(127, "FakeApt: unexpected command")),
        }
    }
}

fn scripted(tag: &'static str, version: &str) -> (AptProvider, ConformanceFixture) {
    let mut fx = ConformanceFixture::new("hello", "ripgrep").backend(FakeApt::new(tag));
    fx.facts = ubuntu_facts(version);
    (AptProvider::without_binary_check(), fx)
}

dots_testkit::conformance_test!(scripted_conformance_2204, { scripted("2204", "22.04") });
dots_testkit::conformance_test!(scripted_conformance_2404, { scripted("2404", "24.04") });

#[test]
fn availability_follows_distro() {
    let p = AptProvider::without_binary_check();
    assert!(matches!(
        p.availability(&ubuntu_facts("24.04")),
        Availability::Ready { bin_path } if bin_path.as_os_str() == "/usr/bin/apt-get"
    ));
    let mac = Facts {
        os: "macos".into(),
        ..Facts::default()
    };
    assert_eq!(p.availability(&mac), Availability::Unsupported);
    let fedora = Facts {
        os: "linux".into(),
        distro: Some("fedora".into()),
        distro_family: Some("fedora".into()),
        ..Facts::default()
    };
    assert_eq!(p.availability(&fedora), Availability::Unsupported);
}

#[test]
fn installed_reports_hold_and_skips_removed() {
    for tag in ["2204", "2404"] {
        let ctx = Ctx {
            runner: FakeApt::new(tag),
            ..Ctx::default()
        };
        let m = AptProvider::new().installed(&ctx).unwrap();
        assert!(m["hello"].pinned, "{tag}: hello is held");
        assert!(!m["bash"].pinned);
        assert!(!m.contains_key("jq"));
    }
}

#[test]
fn installed_is_one_dpkg_query_call() {
    let rec = Arc::new(dots_testkit::RecordingRunner::wrapping(FakeApt::new(
        "2404",
    )));
    let ctx = Ctx {
        runner: rec.clone(),
        ..Ctx::default()
    };
    AptProvider::new().installed(&ctx).unwrap();
    assert_eq!(rec.count(), 1);
    assert!(rec.lines()[0].starts_with("/usr/bin/dpkg-query -W -f="));
}

#[test]
fn latest_parses_candidates_in_one_call() {
    let rec = Arc::new(dots_testkit::RecordingRunner::wrapping(FakeApt::new(
        "2204",
    )));
    let ctx = Ctx {
        runner: rec.clone(),
        ..Ctx::default()
    };
    let specs = [
        Spec::new("jq"),
        Spec::new("hello"),
        Spec::new("nosuchpkg-xyz"),
    ];
    let m = AptProvider::new().latest(&ctx, &specs).unwrap();
    assert_eq!(
        rec.lines(),
        ["/usr/bin/apt-cache policy hello jq nosuchpkg-xyz"]
    );
    assert_eq!(m["jq"].version, "1.6-2.1ubuntu3.2");
    assert_eq!(m["hello"].version, "2.10-2ubuntu4");
    assert!(!m.contains_key("nosuchpkg-xyz"));
}

#[test]
fn plan_is_batched_exact_and_root() {
    let p = AptProvider::new();
    let a = p
        .plan(&[
            op(OpKind::Install, "jq", None),
            op(OpKind::Install, "hello", Some("2.10-2ubuntu4")),
            op(OpKind::Pin, "hello", None),
            op(OpKind::Remove, "bc", None),
            op(OpKind::Upgrade, "curl", None),
            op(OpKind::Unpin, "bash", None),
        ])
        .unwrap();
    let lines: Vec<String> = a.iter().map(|a| a.argv.join(" ")).collect();
    assert_eq!(
        lines,
        [
            "/usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y jq hello=2.10-2ubuntu4",
            "/usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y --only-upgrade curl",
            "/usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get remove -y bc",
            "/usr/bin/apt-mark hold hello",
            "/usr/bin/apt-mark unhold bash",
        ]
    );
    assert!(a.iter().all(|a| a.needs_root));
}

/// A batched install that fails as a whole can be retried one package at a time, so one
/// unknown name fails only itself. A single package, or a repository step, has no retry.
#[test]
fn a_failed_install_batch_offers_per_package_retries() {
    let p = AptProvider::new();
    let a = p
        .plan(&[
            op(OpKind::Install, "jq", None),
            op(OpKind::Install, "no-such-pkg", None),
            op(OpKind::Install, "hello", Some("2.10")),
        ])
        .unwrap();
    assert_eq!(a.len(), 1);
    let each = p.retry_each(&a[0]).expect("a batch of three");
    let lines: Vec<String> = each.iter().map(|a| a.argv.join(" ")).collect();
    assert_eq!(
        lines,
        [
            "/usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y jq",
            "/usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y no-such-pkg",
            "/usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y hello=2.10",
        ]
    );
    assert!(each.iter().all(|a| a.needs_root && a.ops.len() == 1));
    let one = p.plan(&[op(OpKind::Install, "jq", None)]).unwrap();
    assert!(p.retry_each(&one[0]).is_none());
    let rm = p
        .plan(&[op(OpKind::Remove, "a", None), op(OpKind::Remove, "b", None)])
        .unwrap();
    assert!(p.retry_each(&rm[0]).is_none(), "only installs are retried");
}

#[test]
fn plan_rejects_option_injection() {
    let p = AptProvider::new();
    assert!(p.plan(&[op(OpKind::Install, "--purge", None)]).is_err());
    assert!(p.plan(&[op(OpKind::Install, "jq", Some("1 -y"))]).is_err());
}

/// Fails the first install with "Unable to locate package", succeeds after `update`.
#[derive(Debug, Default)]
struct StaleCache(Mutex<Vec<String>>);
impl Runner for StaleCache {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut seen = self.0.lock().unwrap();
        seen.push(cmd.line());
        let updated = seen.iter().any(|l| l.ends_with("apt-get update"));
        Ok(if cmd.args.iter().any(|a| a == "install") && !updated {
            Output::fail(100, "E: Unable to locate package jq\n")
        } else {
            Output::ok("ok\n")
        })
    }
}

/// A package-lists directory that looks fresh (one `*_Packages` file, just written).
fn fresh_lists(dir: &std::path::Path) -> String {
    let lists = dir.join("lists");
    std::fs::create_dir_all(&lists).unwrap();
    std::fs::write(
        lists.join("archive.ubuntu.com_dists_noble_main_Packages"),
        "",
    )
    .unwrap();
    lists.to_string_lossy().into_owned()
}

#[test]
fn exec_refreshes_stale_cache_once_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(StaleCache::default());
    let ctx = Ctx {
        runner: rec.clone(),
        log_dir: dir.path().to_path_buf(),
        env: BTreeMap::from([("DOTS_APT_LISTS".to_string(), fresh_lists(dir.path()))]),
        ..Ctx::default()
    };
    let p = AptProvider::new();
    let actions = p.plan(&[op(OpKind::Install, "jq", None)]).unwrap();
    // The engine may prefix `sudo -n`; the update must carry the same prefix.
    let mut action: Action = actions[0].clone();
    action
        .argv
        .splice(0..0, ["/usr/bin/sudo".to_string(), "-n".to_string()]);
    let out = p.exec(&ctx, &action).unwrap();
    assert!(out.success);
    let seen = rec.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[1].starts_with(
        "/usr/bin/sudo -n /usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get update"
    ));
    let log = std::fs::read_to_string(out.log.unwrap()).unwrap();
    assert!(log.contains("Unable to locate package") && log.contains("retrying"));

    // Offline: no refresh.
    rec.0.lock().unwrap().clear();
    let ctx = Ctx {
        offline: true,
        ..ctx
    };
    assert!(!p.exec(&ctx, &action).unwrap().success);
    assert_eq!(rec.0.lock().unwrap().len(), 1);
}

/// A fresh Ubuntu has no `*_Packages` lists: the first install of a run refreshes them once,
/// with the same `sudo` prefix, and later installs do not.
#[test]
fn first_install_refreshes_missing_package_lists_once() {
    let dir = tempfile::tempdir().unwrap();
    let lists = dir.path().join("lists");
    std::fs::create_dir_all(lists.join("partial")).unwrap();
    std::fs::write(lists.join("lock"), "").unwrap();
    let rec = Arc::new(dots_testkit::RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        log_dir: dir.path().join("logs"),
        env: BTreeMap::from([(
            "DOTS_APT_LISTS".to_string(),
            lists.to_string_lossy().into_owned(),
        )]),
        ..Ctx::default()
    };
    let p = AptProvider::new();
    let sudo = |mut a: Action| {
        a.argv
            .splice(0..0, ["/usr/bin/sudo".to_string(), "-n".to_string()]);
        a
    };
    let a1 = sudo(
        p.plan(&[op(OpKind::Install, "jq", None)])
            .unwrap()
            .remove(0),
    );
    let a2 = sudo(
        p.plan(&[op(OpKind::Install, "fd", None)])
            .unwrap()
            .remove(0),
    );
    assert!(p.exec(&ctx, &a1).unwrap().success);
    assert!(p.exec(&ctx, &a2).unwrap().success);
    let seen: Vec<String> = rec.lines();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(
        seen[0].starts_with(
            "/usr/bin/sudo -n /usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get update"
        ),
        "{seen:?}"
    );
    assert!(seen[1].contains("install -y jq") && seen[2].contains("install -y fd"));

    // Fresh lists: nothing extra. Lists last touched two days ago: refreshed.
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(dots_testkit::RecordingRunner::new());
    let lists = fresh_lists(dir.path());
    let ctx = Ctx {
        runner: rec.clone(),
        log_dir: dir.path().join("logs"),
        env: BTreeMap::from([("DOTS_APT_LISTS".to_string(), lists.clone())]),
        ..Ctx::default()
    };
    let p = AptProvider::new();
    assert!(p.exec(&ctx, &a1).unwrap().success);
    assert_eq!(rec.count(), 1, "fresh lists: no update");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    std::fs::File::open(&lists)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let p = AptProvider::new();
    assert!(p.exec(&ctx, &a1).unwrap().success);
    assert_eq!(rec.count(), 3, "old lists: update then install");
}

#[test]
fn exec_dry_run_runs_nothing() {
    let rec = Arc::new(dots_testkit::RecordingRunner::new());
    let ctx = Ctx {
        dry_run: true,
        runner: rec.clone(),
        ..Ctx::default()
    };
    let p = AptProvider::new();
    let a = p.plan(&[op(OpKind::Install, "jq", None)]).unwrap();
    assert!(p.exec(&ctx, &a[0]).unwrap().success);
    assert_eq!(rec.count(), 0);
}

#[test]
fn owner_of_parses_dpkg_search() {
    let rec = dots_testkit::RecordingRunner::new();
    rec.on(
        &["/usr/bin/dpkg-query", "-S"],
        Output {
            status: 1,
            stdout: fixture("dpkg-S.2404.txt"),
            ..Output::default()
        },
    );
    let ctx = Ctx {
        runner: Arc::new(rec),
        ..Ctx::default()
    };
    let paths = [
        PathBuf::from("/usr/bin/hello"),
        PathBuf::from("/usr/bin/nosuch"),
    ];
    let m = AptProvider::new().owner_of(&ctx, &paths).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[&paths[0]], "hello");
}

// ---- real containers -------------------------------------------------------------------------

fn in_container() -> bool {
    std::env::var("DOTS_APT_CONTAINER").as_deref() == Ok("1")
}

fn real_ctx(dir: &std::path::Path) -> Ctx {
    let _ = std::fs::create_dir_all(dir.join("logs"));
    Ctx {
        runner: Arc::new(RealRunner),
        log_dir: dir.join("logs"),
        home: dir.to_path_buf(),
        env: BTreeMap::new(),
        ..Ctx::default()
    }
}

/// Runs only inside a disposable container (`DOTS_APT_CONTAINER=1`); it really installs.
#[test]
#[ignore = "runs apt for real; only inside the containers driven by container_matrix"]
fn real_conformance() {
    if !in_container() {
        eprintln!("skipped: not in a container");
        return;
    }
    let version = std::env::var("DOTS_APT_VERSION").unwrap_or_default();
    let mut fx = ConformanceFixture::new("bash", "hello").backend(Arc::new(RealRunner));
    fx.facts = ubuntu_facts(&version);
    dots_testkit::conformance::assert_conforms(&AptProvider::new(), &fx);
}

#[test]
#[ignore = "runs apt for real; only inside the containers driven by container_matrix"]
fn real_lifecycle() {
    if !in_container() {
        eprintln!("skipped: not in a container");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = real_ctx(dir.path());
    let p = AptProvider::new();
    let run = |ops: &[Op]| {
        for a in p.plan(ops).unwrap() {
            let o = p.exec(&ctx, &a).unwrap();
            assert!(
                o.success,
                "{:?} failed:\n{}",
                a.argv,
                std::fs::read_to_string(o.log.unwrap()).unwrap()
            );
        }
    };

    // Stale cache: the script leaves the lists empty on purpose, so this exercises update+retry.
    let before = p.installed(&ctx).unwrap();
    assert!(!before.contains_key("jq"));
    run(&[op(OpKind::Install, "jq", None)]);
    let now = p.installed(&ctx).unwrap();
    assert!(!now["jq"].pinned);

    // latest() and exact version, from the real policy.
    let latest = p
        .latest(&ctx, &[Spec::new("jq"), Spec::new("nosuchpkg-xyz")])
        .unwrap();
    assert!(latest.contains_key("jq") && !latest.contains_key("nosuchpkg-xyz"));

    // bins / owner_of.
    let bins = p.bins(&ctx, &now).unwrap();
    let jq_bin = bins.iter().find(|b| b.package == "jq" && b.bin == "jq");
    assert!(jq_bin.is_some(), "bins: {bins:?}");
    let owners = p.owner_of(&ctx, &[PathBuf::from("/usr/bin/jq")]).unwrap();
    assert_eq!(
        owners
            .get(&PathBuf::from("/usr/bin/jq"))
            .map(String::as_str),
        Some("jq")
    );

    // pin / unpin / remove.
    run(&[op(OpKind::Pin, "jq", None)]);
    assert!(p.installed(&ctx).unwrap()["jq"].pinned);
    run(&[op(OpKind::Unpin, "jq", None)]);
    assert!(!p.installed(&ctx).unwrap()["jq"].pinned);
    run(&[op(OpKind::Remove, "jq", None)]);
    assert!(!p.installed(&ctx).unwrap().contains_key("jq"));

    // An exact version that does not exist fails (Limited support), visibly.
    let a = p
        .plan(&[op(OpKind::Install, "jq", Some("0.0.0-nope"))])
        .unwrap();
    assert!(!p.exec(&ctx, &a[0]).unwrap().success);
}

/// Host-side driver. `DOTS_CONTAINER_TESTS=1 cargo test -p dots-providers --test apt
/// container_matrix -- --nocapture`.
#[test]
fn container_matrix() {
    if std::env::var("DOTS_CONTAINER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set DOTS_CONTAINER_TESTS=1 to run the Ubuntu container matrix");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap();
    for ver in ["22.04", "24.04"] {
        let status = std::process::Command::new("docker")
            .args(["run", "--rm", "--platform", "linux/arm64"])
            .args(["-v", &format!("{}:/work:ro", root.display())])
            .args(["-v", &format!("dots-apt-cache-{ver}:/cache")])
            .args(["-e", &format!("DOTS_APT_VERSION={ver}")])
            .arg(format!("ubuntu:{ver}"))
            .args([
                "/bin/sh",
                "/work/crates/dots-providers/tests/containers/apt/run.sh",
            ])
            .status()
            .expect("docker");
        assert!(status.success(), "ubuntu:{ver} container run failed");
    }
}

// ------------------------------------------------------------------ apt repositories

mod repos {
    use super::*;
    use dots_providers::{Fetcher, ProviderError};
    use std::path::Path;

    const FP: &str = "BC528686B50D79E339D3721CEB3E94ADBE1229CF";
    const KEY: &str =
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQINBFYk\n-----END PGP PUBLIC KEY BLOCK-----\n";

    #[derive(Debug, Default)]
    struct FakeFetcher {
        files: BTreeMap<String, String>,
        /// Non-text bodies (a dearmored key).
        raw: BTreeMap<String, Vec<u8>>,
        calls: Mutex<Vec<String>>,
    }
    impl Fetcher for FakeFetcher {
        fn fetch(&self, url: &str) -> Result<Vec<u8>> {
            self.calls.lock().unwrap().push(url.to_string());
            if let Some(b) = self.raw.get(url) {
                return Ok(b.clone());
            }
            self.files
                .get(url)
                .map(|b| b.clone().into_bytes())
                .ok_or_else(|| ProviderError::Other(format!("404 {url}")))
        }
    }

    /// Fakes gpg and apt-get, answers the release codename, and runs the root scripts for
    /// real against a temp dir standing in for `/etc/apt`.
    #[derive(Debug)]
    struct Sim {
        etc: PathBuf,
        gpg: String,
        seen: Mutex<Vec<String>>,
        /// Number of upcoming `apt-get update` runs that fail.
        update_failures: std::sync::atomic::AtomicUsize,
        /// Stdin of every root repo script run.
        script_stdin: Mutex<Vec<dots_providers::Stdin>>,
    }
    impl Runner for Sim {
        fn run(&self, cmd: &Cmd) -> Result<Output> {
            self.seen.lock().unwrap().push(cmd.line());
            // Pretend to be sudo: run what follows it.
            if cmd.program.to_string_lossy() == "/usr/bin/sudo" {
                let mut inner = Cmd::from_argv(&cmd.args[1..]).unwrap();
                inner.env = cmd.env.clone();
                inner.stdin = cmd.stdin.clone();
                let out = self.run(&inner);
                self.seen.lock().unwrap().pop();
                return out;
            }
            let prog = cmd.program.to_string_lossy().into_owned();
            let is_script = cmd.args.iter().any(|a| a == "dots-apt-repo");
            if prog == "/usr/bin/gpg" {
                return Ok(Output::ok(&self.gpg));
            }
            if prog == "/bin/sh" && is_script {
                self.script_stdin.lock().unwrap().push(cmd.stdin.clone());
                let mut c = cmd.clone();
                for a in &mut c.args {
                    *a = a.replace("/etc/apt", &self.etc.to_string_lossy());
                }
                return RealRunner.run(&c);
            }
            if prog == "/bin/sh" {
                return Ok(Output::ok("noble"));
            }
            if cmd.args.iter().any(|a| a == "update") {
                use std::sync::atomic::Ordering;
                let left = self.update_failures.load(Ordering::SeqCst);
                if left > 0 {
                    self.update_failures.store(left - 1, Ordering::SeqCst);
                    return Ok(Output::fail(100, "E: Failed to fetch"));
                }
            }
            Ok(Output::ok("ok\n"))
        }
    }
    impl Sim {
        fn calls(&self, needle: &str) -> usize {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|l| l.contains(needle))
                .count()
        }
    }

    struct Box_ {
        _tmp: tempfile::TempDir,
        etc: PathBuf,
        sim: Arc<Sim>,
        fetcher: Arc<FakeFetcher>,
        ctx: Ctx,
    }

    fn sandbox(gpg_fp: &str) -> Box_ {
        let tmp = tempfile::tempdir().unwrap();
        let etc = tmp.path().join("etc-apt");
        let gpg =
            format!("pub:-:4096:1:EB3E94ADBE1229CF:1:::-:::scESC:::::::\nfpr:::::::::{gpg_fp}:\n");
        let sim = Arc::new(Sim {
            etc: etc.clone(),
            gpg,
            seen: Mutex::new(Vec::new()),
            update_failures: Default::default(),
            script_stdin: Mutex::new(Vec::new()),
        });
        let mut f = FakeFetcher::default();
        f.files.insert(MS_KEY.into(), KEY.into());
        f.files.insert(
            "https://api.launchpad.net/devel/~git-core/+archive/ubuntu/ppa".into(),
            format!("{{\"signing_key_fingerprint\": \"{}\"}}", FP.to_lowercase()),
        );
        f.files.insert(
            format!("https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x{FP}"),
            KEY.into(),
        );
        let fetcher = Arc::new(f);
        let ctx = Ctx {
            runner: sim.clone(),
            fetcher: fetcher.clone(),
            cache_dir: tmp.path().join("cache"),
            log_dir: tmp.path().join("logs"),
            env: BTreeMap::from([("DOTS_APT_LISTS".to_string(), fresh_lists(tmp.path()))]),
            ..Ctx::default()
        };
        Box_ {
            _tmp: tmp,
            etc,
            sim,
            fetcher,
            ctx,
        }
    }

    const MS_KEY: &str = "https://packages.microsoft.com/keys/microsoft.asc";

    fn ppa_op(kind: OpKind, name: &str) -> Op {
        Op {
            kind,
            spec: Spec::new(name).opt("repo.ppa", "git-core/ppa"),
            version: None,
        }
    }

    fn code_op(kind: OpKind, name: &str, pin: bool) -> Op {
        let mut spec = Spec::new(name)
            .opt("repo.url", "https://packages.microsoft.com/repos/code")
            .opt("repo.suite", "stable")
            .opt("repo.components", "main")
            .opt("repo.key", MS_KEY)
            .opt("repo.fingerprint", FP);
        if pin {
            spec = spec.opt("pin.origin", "packages.microsoft.com");
        }
        Op {
            kind,
            spec,
            version: None,
        }
    }

    /// What the engine does: root actions get a `sudo -n` prefix.
    fn exec_all(
        p: &AptProvider,
        ctx: &Ctx,
        actions: &[Action],
    ) -> Vec<Result<dots_providers::Outcome>> {
        actions
            .iter()
            .map(|a| {
                let mut a = a.clone();
                a.argv
                    .splice(0..0, ["/usr/bin/sudo".to_string(), "-n".to_string()]);
                p.exec(ctx, &a)
            })
            .collect()
    }

    fn exists(root: &Path, rel: &str) -> bool {
        root.join(rel).is_file()
    }

    /// An adopted package that declares a repo whose sources file is missing is "stale"; once
    /// the file exists it is not. Detection reads `DOTS_APT_SOURCES_DIR`.
    #[test]
    fn declared_repo_missing_makes_an_installed_package_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Ctx {
            env: BTreeMap::from([(
                "DOTS_APT_SOURCES_DIR".to_string(),
                tmp.path().display().to_string(),
            )]),
            ..Ctx::default()
        };
        let p = AptProvider::without_binary_check();
        let spec = Spec::new("git").opt("repo.ppa", "git-core/ppa");
        assert_eq!(
            p.stale_reason(&ctx, &spec).as_deref(),
            Some("repo git-core/ppa not configured")
        );
        // No repo declared: never stale.
        assert_eq!(p.stale_reason(&ctx, &Spec::new("jq")), None);
        std::fs::write(tmp.path().join("dots-ppa-git-core-ppa.sources"), "x").unwrap();
        assert_eq!(p.stale_reason(&ctx, &spec), None);
        // Without the override a binary-less provider does not probe the real /etc/apt.
        assert_eq!(p.stale_reason(&Ctx::default(), &spec), None);
    }

    /// The upgrade the engine plans for a stale package carries the repository add and update.
    #[test]
    fn upgrade_of_a_ppa_package_adds_the_repo_first() {
        let acts = AptProvider::new()
            .plan(&[ppa_op(OpKind::Upgrade, "git")])
            .unwrap();
        let lines: Vec<String> = acts.iter().map(|a| a.argv.join(" ")).collect();
        assert!(
            lines[0].starts_with("dots:apt-repo add id=ppa-git-core-ppa"),
            "{lines:?}"
        );
        assert!(lines[1].contains("apt-get update"), "{lines:?}");
        assert!(lines[2].contains("git"), "{lines:?}");
    }

    #[test]
    fn ppa_packages_install_after_one_update() {
        let p = AptProvider::new();
        let actions = p
            .plan(&[
                ppa_op(OpKind::Install, "git"),
                ppa_op(OpKind::Install, "git-lfs"),
            ])
            .unwrap();
        let lines: Vec<String> = actions.iter().map(|a| a.argv.join(" ")).collect();
        assert_eq!(actions.len(), 3, "{lines:?}");
        assert!(lines[0].starts_with("dots:apt-repo add id=ppa-git-core-ppa ppa=git-core/ppa"));
        assert!(lines[0].contains("file=/etc/apt/sources.list.d/dots-ppa-git-core-ppa.sources"));
        assert!(lines[1].ends_with("/usr/bin/apt-get update"));
        assert!(lines[2].ends_with("apt-get install -y git git-lfs"));
        assert!(actions.iter().all(|a| a.needs_root));
        // Repository steps carry only their own packages' ops.
        assert!(
            actions[..3]
                .iter()
                .all(|a| a.ops.len() == 2 || a.ops.len() == 1)
        );
        assert!(
            actions[0]
                .ops
                .iter()
                .all(|o| o.spec.name.starts_with("git"))
        );
    }

    #[test]
    fn ppa_exec_writes_owned_files_then_updates_once() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let actions = p.plan(&[ppa_op(OpKind::Install, "git")]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        let src =
            std::fs::read_to_string(b.etc.join("sources.list.d/dots-ppa-git-core-ppa.sources"))
                .unwrap();
        assert!(src.starts_with("# Managed by dots"), "{src}");
        assert!(src.contains("URIs: https://ppa.launchpadcontent.net/git-core/ppa/ubuntu"));
        assert!(src.contains("Suites: noble"));
        // (the sim stands a temp dir in for /etc/apt everywhere)
        let keyring = b.etc.join("keyrings/dots-ppa-git-core-ppa.asc");
        assert!(src.contains(&format!("Signed-By: {}", keyring.display())));
        assert!(exists(&b.etc, "keyrings/dots-ppa-git-core-ppa.asc"));
        assert_eq!(b.sim.calls("apt-get update"), 1);
        assert_eq!(b.sim.calls("/usr/bin/apt-key"), 0);
        // Everything privileged carried the sudo prefix.
        assert!(
            b.sim
                .seen
                .lock()
                .unwrap()
                .iter()
                .filter(|l| l.contains("dots-apt-repo"))
                .all(|l| l.starts_with("/usr/bin/sudo -n /bin/sh -c"))
        );
    }

    #[test]
    fn third_party_repository_lists_its_files() {
        let p = AptProvider::new();
        let a = p.plan(&[code_op(OpKind::Install, "code", true)]).unwrap();
        let add = a[0].argv.join(" ");
        for f in [
            "file=/etc/apt/keyrings/dots-packages.microsoft.com-repos-code-stable.asc",
            "file=/etc/apt/sources.list.d/dots-packages.microsoft.com-repos-code-stable.sources",
            "file=/etc/apt/preferences.d/dots-packages.microsoft.com-repos-code-stable",
        ] {
            assert!(add.contains(f), "{add}");
        }
        let no_pin = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        assert!(!no_pin[0].argv.join(" ").contains("preferences.d"));
    }

    #[test]
    fn pin_writes_a_preferences_file() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", true)]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        let prefs = std::fs::read_to_string(
            b.etc
                .join("preferences.d/dots-packages.microsoft.com-repos-code-stable"),
        )
        .unwrap();
        assert!(
            prefs.contains("Pin: origin packages.microsoft.com")
                && prefs.contains("Pin-Priority: 1000")
        );
        let src = std::fs::read_to_string(
            b.etc
                .join("sources.list.d/dots-packages.microsoft.com-repos-code-stable.sources"),
        )
        .unwrap();
        assert!(src.contains("Suites: stable") && src.contains("Components: main"));
    }

    #[test]
    fn unchanged_repository_does_not_update() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        assert_eq!(b.sim.calls("apt-get update"), 1);
        // A second run with the files already in place.
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        assert_eq!(b.sim.calls("apt-get update"), 1, "no second update");
        // Changing the repository (suite) updates again.
        let mut changed = code_op(OpKind::Install, "code", false);
        changed.spec = changed.spec.opt("repo.components", "main,contrib");
        let actions = p.plan(&[changed]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        assert_eq!(b.sim.calls("apt-get update"), 2);
    }

    #[test]
    fn fingerprint_mismatch_refuses_and_writes_nothing() {
        let b = sandbox("0000000000000000000000000000000000000000");
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", true)]).unwrap();
        let res = exec_all(&p, &b.ctx, &actions);
        match &res[0] {
            Err(ProviderError::Coded { code, message, .. }) => {
                assert_eq!(*code, "integrity-mismatch");
                assert!(
                    message.contains(FP) && message.contains("0000000000"),
                    "{message}"
                );
            }
            other => panic!("expected integrity-mismatch, got {other:?}"),
        }
        assert!(!b.etc.exists(), "nothing written under /etc/apt");
        assert_eq!(b.sim.calls("dots-apt-repo"), 0, "no root command ran");
        assert!(
            !b.ctx
                .cache_dir
                .join("apt-keys/packages.microsoft.com-repos-code-stable.asc")
                .exists()
        );
    }

    #[test]
    fn key_that_is_not_armored_is_refused() {
        let mut b = sandbox(FP);
        let mut f = FakeFetcher::default();
        f.files.insert(MS_KEY.into(), "\u{1}binary".into());
        b.ctx.fetcher = Arc::new(f);
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        assert!(exec_all(&p, &b.ctx, &actions)[0].is_err());
        assert!(!b.etc.exists());
    }

    /// A dearmored (binary) key is accepted: gpg still checks the fingerprint, the key is
    /// stored as a `.gpg` keyring, `Signed-By` names it, and the same bytes reach root on stdin.
    #[test]
    fn binary_key_is_accepted_and_stored_as_a_gpg_keyring() {
        let mut b = sandbox(FP);
        // An OpenPGP public-key packet header (new format, tag 6) and some bytes.
        let bin: Vec<u8> = vec![0xC6, 0x0D, 0x04, 0x00, 0xFF, 0x80, 0x01];
        let mut f = FakeFetcher::default();
        f.raw.insert(MS_KEY.into(), bin.clone());
        b.ctx.fetcher = Arc::new(f);
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        let id = "packages.microsoft.com-repos-code-stable";
        let kept = std::fs::read(b.etc.join(format!("keyrings/dots-{id}.gpg"))).unwrap();
        assert_eq!(kept, bin);
        assert!(!exists(&b.etc, &format!("keyrings/dots-{id}.asc")));
        let sources =
            std::fs::read_to_string(b.etc.join(format!("sources.list.d/dots-{id}.sources")))
                .unwrap();
        // (The harness maps `/etc/apt` to a temp dir.)
        assert!(
            sources
                .trim_end()
                .ends_with(&format!("keyrings/dots-{id}.gpg")),
            "{sources}"
        );
        assert_eq!(
            b.sim.script_stdin.lock().unwrap()[0],
            dots_providers::Stdin::Bytes(bin)
        );
        assert!(b.sim.calls("--show-keys") >= 1, "gpg verified it");

        // The fingerprint is checked for a binary key too.
        let mut bad = sandbox("1111111111111111111111111111111111111111");
        let mut f = FakeFetcher::default();
        f.raw.insert(MS_KEY.into(), vec![0xC6, 0x01, 0x02]);
        bad.ctx.fetcher = Arc::new(f);
        assert!(exec_all(&p, &bad.ctx, &actions)[0].is_err());
        assert!(!bad.etc.exists());

        // Re-running with an armored key swaps the keyring for the `.asc` one.
        let arm = sandbox(FP);
        let p = AptProvider::new();
        let a2 = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        // Same repository dir: reuse the first sandbox's `/etc/apt` stand-in.
        let mut ctx2 = arm.ctx.clone();
        ctx2.runner = Arc::new(Sim {
            etc: b.etc.clone(),
            gpg: format!("pub:-:4096:1:EB3E94ADBE1229CF:1:::-:::scESC:::::::\nfpr:::::::::{FP}:\n"),
            seen: Mutex::new(Vec::new()),
            update_failures: Default::default(),
            script_stdin: Mutex::new(Vec::new()),
        });
        for r in exec_all(&p, &ctx2, &a2) {
            let o = r.unwrap();
            assert!(
                o.success,
                "{}",
                o.log
                    .and_then(|l| std::fs::read_to_string(l).ok())
                    .unwrap_or_default()
            );
        }
        assert!(exists(&b.etc, &format!("keyrings/dots-{id}.asc")));
        assert!(!exists(&b.etc, &format!("keyrings/dots-{id}.gpg")));
    }

    /// A flat repository (`deb https://download.sublimetext.com/ apt/stable/`) writes a sources
    /// file with no `Components`.
    #[test]
    fn flat_repository_is_written_without_components() {
        let mut b = sandbox(FP);
        let mut f = FakeFetcher::default();
        f.files.insert(
            "https://download.sublimetext.com/sublimehq-pub.gpg".into(),
            KEY.into(),
        );
        b.ctx.fetcher = Arc::new(f);
        let p = AptProvider::new();
        let op = Op {
            kind: OpKind::Install,
            spec: Spec::new("sublime-text")
                .opt("repo.url", "https://download.sublimetext.com/")
                .opt("repo.suite", "apt/stable/")
                .opt(
                    "repo.key",
                    "https://download.sublimetext.com/sublimehq-pub.gpg",
                )
                .opt("repo.fingerprint", FP),
            version: None,
        };
        let actions = p.plan(&[op]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        let sources = std::fs::read_to_string(
            b.etc
                .join("sources.list.d/dots-download.sublimetext.com-apt-stable.sources"),
        )
        .unwrap();
        assert!(
            sources.contains("URIs: https://download.sublimetext.com/\n"),
            "{sources}"
        );
        assert!(sources.contains("Suites: apt/stable/\n"), "{sources}");
        assert!(!sources.contains("Components"), "{sources}");
    }

    #[test]
    fn offline_cannot_fetch_a_key() {
        let mut b = sandbox(FP);
        b.ctx.offline = true;
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        assert!(exec_all(&p, &b.ctx, &actions)[0].is_err());
        assert!(b.fetcher.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn repository_goes_with_its_last_package() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let install = p
            .plan(&[
                code_op(OpKind::Install, "code", true),
                code_op(OpKind::Install, "code-insiders", true),
            ])
            .unwrap();
        for r in exec_all(&p, &b.ctx, &install) {
            assert!(r.unwrap().success);
        }
        let key = "keyrings/dots-packages.microsoft.com-repos-code-stable.asc";
        let src = "sources.list.d/dots-packages.microsoft.com-repos-code-stable.sources";
        let pref = "preferences.d/dots-packages.microsoft.com-repos-code-stable";
        assert!(exists(&b.etc, key) && exists(&b.etc, src) && exists(&b.etc, pref));

        // One of two goes: another wanted package still names the repository.
        let mut first = code_op(OpKind::Remove, "code", true);
        first.spec = first.spec.opt("repo_keep", "true");
        let plan = p.plan(&[first]).unwrap();
        assert_eq!(plan.len(), 1, "only the package removal");
        for r in exec_all(&p, &b.ctx, &plan) {
            assert!(r.unwrap().success);
        }
        assert!(exists(&b.etc, key) && exists(&b.etc, src) && exists(&b.etc, pref));

        // The last one goes: the repository files go with it.
        let plan = p
            .plan(&[code_op(OpKind::Remove, "code-insiders", true)])
            .unwrap();
        assert_eq!(plan.len(), 2);
        assert!(
            plan[1]
                .argv
                .join(" ")
                .starts_with("dots:apt-repo remove id=packages.microsoft.com-repos-code-stable")
        );
        for r in exec_all(&p, &b.ctx, &plan) {
            assert!(r.unwrap().success);
        }
        assert!(!exists(&b.etc, key) && !exists(&b.etc, src) && !exists(&b.etc, pref));
    }

    #[test]
    fn removal_leaves_a_sources_file_dots_did_not_write() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let src = b
            .etc
            .join("sources.list.d/dots-packages.microsoft.com-repos-code-stable.sources");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "Types: deb\n").unwrap();
        let plan = p.plan(&[code_op(OpKind::Remove, "code", false)]).unwrap();
        for r in exec_all(&p, &b.ctx, &plan) {
            assert!(r.unwrap().success);
        }
        assert!(
            src.is_file(),
            "a file without dots' mark is not ours to delete"
        );
    }

    #[test]
    fn replacing_a_package_keeps_the_repository() {
        // Removing one package while installing another from the same repository.
        let p = AptProvider::new();
        let plan = p
            .plan(&[
                code_op(OpKind::Remove, "code", false),
                code_op(OpKind::Install, "code-insiders", false),
            ])
            .unwrap();
        assert!(
            !plan
                .iter()
                .any(|a| a.argv.join(" ").contains("apt-repo remove"))
        );
    }

    #[test]
    fn bad_repository_declarations_fail_at_plan_time() {
        let p = AptProvider::new();
        let mut bad = code_op(OpKind::Install, "code", false);
        bad.spec = bad.spec.opt("repo.fingerprint", "not hex");
        assert!(p.plan(&[bad]).is_err());
    }

    /// The verified key reaches the root script on stdin; no path in argv or under the
    /// cache can be swapped between the fingerprint check and the root copy.
    #[test]
    fn verified_key_is_handed_to_root_on_stdin_not_by_path() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let actions = p.plan(&[code_op(OpKind::Install, "code", false)]).unwrap();
        for r in exec_all(&p, &b.ctx, &actions) {
            assert!(r.unwrap().success);
        }
        let stdins = b.sim.script_stdin.lock().unwrap();
        assert_eq!(stdins.len(), 1);
        assert_eq!(
            stdins[0],
            dots_providers::Stdin::Bytes(KEY.as_bytes().to_vec())
        );
        let cache = b.ctx.cache_dir.to_string_lossy().into_owned();
        assert!(
            !b.sim
                .seen
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains(&cache)),
            "no command mentions a cache path for the key"
        );
        assert!(
            !b.ctx.cache_dir.join("apt-keys").exists(),
            "the key is never written to the cache"
        );
        let installed = std::fs::read(
            b.etc
                .join("keyrings/dots-packages.microsoft.com-repos-code-stable.asc"),
        )
        .unwrap();
        assert_eq!(installed, KEY.as_bytes());
    }

    /// `apt-get update` failing once must not be forgotten: the next run retries it even
    /// though the repository files are already in place.
    #[test]
    fn failed_update_is_retried_on_the_next_run() {
        let b = sandbox(FP);
        b.sim
            .update_failures
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let actions = AptProvider::new()
            .plan(&[code_op(OpKind::Install, "code", false)])
            .unwrap();
        let first = exec_all(&AptProvider::new(), &b.ctx, &actions);
        assert!(!first[1].as_ref().unwrap().success, "update failed");
        assert_eq!(b.sim.calls("apt-get update"), 1);
        // A new process (fresh provider): files unchanged, but the update is still owed.
        let second = exec_all(&AptProvider::new(), &b.ctx, &actions);
        assert!(second[1].as_ref().unwrap().success);
        assert_eq!(b.sim.calls("apt-get update"), 2, "update retried");
        // And once it has succeeded it is not repeated.
        exec_all(&AptProvider::new(), &b.ctx, &actions);
        assert_eq!(b.sim.calls("apt-get update"), 2);
    }

    /// One repository whose key does not verify skips only its own packages.
    #[test]
    fn bad_repository_skips_only_its_own_packages() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let mut bad = code_op(OpKind::Install, "badpkg", false);
        bad.spec = bad
            .spec
            .opt("repo.suite", "other")
            .opt("repo.fingerprint", &"1".repeat(40));
        let jq = Op {
            kind: OpKind::Install,
            spec: Spec::new("jq"),
            version: None,
        };
        let actions = p.plan(&[jq, ppa_op(OpKind::Install, "git"), bad]).unwrap();
        // Every repository step names only its own packages.
        for a in &actions {
            let names: Vec<&str> = a.ops.iter().map(|o| o.spec.name.as_str()).collect();
            assert!(
                names.is_empty() || names == ["jq"] || names == ["git"] || names == ["badpkg"],
                "{names:?}"
            );
            assert!(
                !names.is_empty(),
                "no action without its packages: {:?}",
                a.argv
            );
        }
        let results = exec_all(&p, &b.ctx, &actions);
        let ran = |needle: &str| b.sim.calls(needle);
        assert_eq!(ran("install -y jq"), 1, "archive package unaffected");
        assert_eq!(ran("install -y git"), 1, "other repository unaffected");
        assert_eq!(
            ran("install -y badpkg"),
            0,
            "bad repository's package skipped"
        );
        let failed = actions
            .iter()
            .zip(&results)
            .filter(|(a, r)| {
                a.argv.iter().any(|x| x == "badpkg") && !matches!(r, Ok(o) if o.success)
            })
            .count();
        assert!(failed >= 1, "the skipped install reports failure");
    }

    /// Holding or releasing a package of a repository that could not be prepared is skipped
    /// too, not run and failed; a package from a healthy repository is still held.
    #[test]
    fn pin_and_unpin_of_a_failed_repository_are_skipped() {
        let b = sandbox(FP);
        let p = AptProvider::new();
        let bad = |kind, name: &str| {
            let mut o = code_op(kind, name, false);
            o.spec = o
                .spec
                .opt("repo.suite", "other")
                .opt("repo.fingerprint", &"1".repeat(40));
            o
        };
        let actions = p
            .plan(&[
                bad(OpKind::Install, "badpkg"),
                bad(OpKind::Pin, "badpkg"),
                bad(OpKind::Unpin, "badother"),
                op(OpKind::Pin, "jq", None),
            ])
            .unwrap();
        let results = exec_all(&p, &b.ctx, &actions);
        assert_eq!(b.sim.calls("apt-mark hold jq"), 1, "healthy pin still runs");
        assert_eq!(b.sim.calls("apt-mark hold badpkg"), 0);
        assert_eq!(b.sim.calls("apt-mark unhold badother"), 0);
        let skipped = actions
            .iter()
            .zip(&results)
            .filter(|(a, _)| a.argv.iter().any(|x| x == "badpkg" || x == "badother"))
            .filter(|(a, _)| matches!(a.kind, OpKind::Pin | OpKind::Unpin))
            .all(|(_, r)| !matches!(r, Ok(o) if o.success));
        assert!(skipped, "skipped holds report failure");
    }

    /// The engine tells the provider which specs stay wanted; a repository goes only with
    /// its last user, across batches.
    #[test]
    fn plan_with_wanted_keeps_a_repository_that_others_still_use() {
        let p = AptProvider::new();
        let remove = code_op(OpKind::Remove, "code", false);
        let other = code_op(OpKind::Install, "code-insiders", false).spec;
        let users = p.repo_users(std::slice::from_ref(&other));
        assert_eq!(
            users["packages.microsoft.com-repos-code-stable"],
            ["code-insiders"]
        );
        let kept = p
            .plan_with_wanted(std::slice::from_ref(&remove), std::slice::from_ref(&other))
            .unwrap();
        assert_eq!(kept.len(), 1, "only the package removal: {kept:?}");
        let dropped = p.plan_with_wanted(&[remove], &[]).unwrap();
        assert!(
            dropped
                .iter()
                .any(|a| a.argv.join(" ").contains("apt-repo remove"))
        );
    }
}

#[test]
fn roots_come_from_apt_mark_showmanual() {
    let rec = Arc::new(dots_testkit::RecordingRunner::new());
    rec.on(&[], Output::ok("curl\nlibc6:amd64\n\nvim\n"));
    let ctx = Ctx {
        runner: rec.clone(),
        ..Ctx::default()
    };
    let roots = AptProvider::new().roots(&ctx).unwrap().unwrap();
    assert_eq!(rec.lines(), ["apt-mark showmanual"]);
    assert_eq!(
        roots.into_iter().collect::<Vec<_>>(),
        ["curl", "libc6", "vim"]
    );
}

/// Answers `apt-get -s remove` with a simulated plan that takes a reverse dependency along.
#[derive(Debug, Default)]
struct SimRemove(Mutex<Vec<String>>);
impl Runner for SimRemove {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        self.0.lock().unwrap().push(cmd.line());
        assert!(cmd.args.iter().any(|a| a == "-s"), "{}", cmd.line());
        Ok(Output::ok(
            "Reading package lists...\nThe following packages will be REMOVED:\n  jq libjq1\n\
             Remv jq [1.6-2.1]\nRemv libjq1:amd64 [1.6-2.1]\nRemv other [2]\n",
        ))
    }
}

#[test]
fn removal_collateral_lists_the_other_packages_a_simulated_remove_would_take() {
    let sim = Arc::new(SimRemove::default());
    let ctx = Ctx {
        runner: sim.clone(),
        ..Ctx::default()
    };
    let p = AptProvider::new();
    let plan = p.plan(&[op(OpKind::Remove, "jq", None)]).unwrap();
    let extra = p.removal_collateral(&ctx, &plan[0]).unwrap();
    assert_eq!(extra, ["libjq1:amd64", "other"]);
    assert_eq!(
        sim.0.lock().unwrap().len(),
        1,
        "nothing but the simulation ran"
    );
    // Installs are not simulated.
    let plan = p.plan(&[op(OpKind::Install, "jq", None)]).unwrap();
    assert!(p.removal_collateral(&ctx, &plan[0]).unwrap().is_empty());
}
