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

#[test]
fn plan_rejects_option_injection_and_ppa() {
    let p = AptProvider::new();
    assert!(p.plan(&[op(OpKind::Install, "--purge", None)]).is_err());
    assert!(p.plan(&[op(OpKind::Install, "jq", Some("1 -y"))]).is_err());
    let mut o = op(OpKind::Install, "nvim", None);
    o.spec.source = Some("ppa:x/y".into());
    assert!(p.plan(&[o]).is_err());
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

#[test]
fn exec_refreshes_stale_cache_once_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(StaleCache::default());
    let ctx = Ctx {
        runner: rec.clone(),
        log_dir: dir.path().to_path_buf(),
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
