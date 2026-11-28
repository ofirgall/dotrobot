use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dots_facts::Facts;
use dots_providers::cargo::CargoProvider;
use dots_providers::{
    Availability, Cmd, Ctx, Op, OpKind, Output, Provider, RealRunner, Result, Runner, Spec,
};
use dots_testkit::{ConformanceFixture, RecordingRunner, conformance_test};

const LIST: &str = include_str!("fixtures/cargo/install-list.txt");
const CRATES2: &str = include_str!("fixtures/cargo/crates2.json");

/// A tiny in-memory cargo: `install --list` and `install [flags] name@ver`.
#[derive(Debug, Default)]
struct FakeCargo(Mutex<BTreeMap<String, String>>, PathBuf);

impl FakeCargo {
    fn write(&self, s: &BTreeMap<String, String>) {
        let installs: Vec<String> = s
            .iter()
            .map(|(n, v)| format!("\"{n} {v} (registry+https://github.com/rust-lang/crates.io-index)\":{{\"bins\":[\"{n}\"]}}"))
            .collect();
        std::fs::write(
            &self.1,
            format!("{{\"installs\":{{{}}}}}", installs.join(",")),
        )
        .unwrap();
    }
}

impl Runner for FakeCargo {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut s = self.0.lock().unwrap();
        if cmd.args == ["install", "--list"] {
            let text: String = s
                .iter()
                .map(|(n, v)| format!("{n} v{v}:\n    {n}\n"))
                .collect();
            return Ok(Output::ok(&text));
        }
        if cmd.args.first().map(String::as_str) == Some("install") {
            let target = cmd.args.last().unwrap();
            let (n, v) = target.split_once('@').unwrap_or((target, "9.9.9"));
            s.insert(n.to_string(), v.to_string());
            self.write(&s);
            return Ok(Output::ok("installed\n"));
        }
        Ok(Output::fail(2, "unexpected"))
    }
}

conformance_test!(cargo_conforms, {
    let fx = ConformanceFixture::new("present", "absent").exact_version("2.0.0");
    let bin = fx.sandbox_dir().join("home/.cargo/bin/cargo");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let fake = FakeCargo(
        Mutex::default(),
        fx.sandbox_dir().join("home/.cargo/.crates2.json"),
    );
    let init = BTreeMap::from([("present".to_string(), "1.0.0".to_string())]);
    fake.write(&init);
    *fake.0.lock().unwrap() = init;
    let fx = fx.backend(Arc::new(fake));
    (CargoProvider::for_ctx(&fx.ctx()), fx)
});

/// `k=v;k=v` into spec options.
fn kv_spec(name: &str, kv: Option<&str>) -> Spec {
    let mut s = Spec::new(name);
    for pair in kv.unwrap_or("").split(';') {
        if let Some((k, v)) = pair.split_once('=') {
            s = s.opt(k.trim(), v.trim());
        }
    }
    s
}

fn op(kind: OpKind, name: &str, source: Option<&str>, version: Option<&str>) -> Op {
    Op {
        kind,
        spec: kv_spec(name, source),
        version: version.map(str::to_string),
    }
}

fn argv(p: &CargoProvider, o: Op) -> Vec<String> {
    p.plan(&[o]).unwrap().remove(0).argv
}

#[test]
fn plan_flags() {
    let p = CargoProvider::with_bin("/c/cargo");
    assert_eq!(
        argv(&p, op(OpKind::Install, "fd-find", None, Some("10.2.0"))),
        ["/c/cargo", "install", "--locked", "fd-find@10.2.0"]
    );
    assert_eq!(
        argv(
            &p,
            op(
                OpKind::Install,
                "demo",
                Some("git=https://x/y;rev=abc123;features=a,b;locked=false"),
                None
            )
        ),
        [
            "/c/cargo",
            "install",
            "--git",
            "https://x/y",
            "--rev",
            "abc123",
            "--features",
            "a,b",
            "demo"
        ]
    );
    assert_eq!(
        argv(&p, op(OpKind::Remove, "demo", None, None)),
        ["/c/cargo", "uninstall", "demo"]
    );
    assert!(
        p.plan(&[op(
            OpKind::Install,
            "demo",
            Some("git=https://x/y"),
            Some("1.0.0")
        )])
        .is_err()
    );
}

#[test]
fn installed_reads_crates2_json_with_zero_calls() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
    std::fs::write(
        tmp.path().join(".cargo/.crates2.json"),
        CRATES2.replace(
            "other 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)",
            "demo 0.3.0 (git+https://github.com/example/demo#abc123)",
        ),
    )
    .unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        ..Ctx::default()
    };
    let m = CargoProvider::new().installed(&ctx).unwrap();
    assert_eq!(rec.count(), 0);
    assert_eq!(m["bottom"].version, "0.14.4");
    assert_eq!(m["bottom"].source, None);
    assert_eq!(
        m["demo"].source.as_deref(),
        Some("https://github.com/example/demo#abc123")
    );
}

#[test]
fn installed_falls_back_to_install_list_once() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let bin = home.join(".cargo/bin/cargo");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &[bin.to_str().unwrap(), "install", "--list"],
        Output::ok(LIST),
    );
    let ctx = Ctx {
        runner: rec.clone(),
        home,
        ..Ctx::default()
    };
    let m = CargoProvider::for_ctx(&ctx).installed(&ctx).unwrap();
    assert_eq!(rec.count(), 1);
    assert_eq!(m["bottom"].version, "0.14.4");
    assert_eq!(
        m["dots-demo"].source.as_deref(),
        Some("https://github.com/example/dots-demo#0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(m.len(), 4);
}

/// A plan made with `new()` (bare `cargo`) still executes through the resolved binary.
#[test]
fn exec_resolves_bare_program() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".cargo/bin/cargo");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let p = CargoProvider::new();
    let a = p
        .plan(&[op(OpKind::Install, "x", None, None)])
        .unwrap()
        .remove(0);
    assert_eq!(a.argv[0], "cargo");
    assert!(p.exec(&ctx, &a).unwrap().success);
    assert_eq!(rec.commands()[0].program, bin);
}

#[test]
fn bins_from_crates2_json() {
    let tmp = tempfile::tempdir().unwrap();
    let ch = tmp.path().join("ch");
    std::fs::create_dir_all(&ch).unwrap();
    std::fs::write(ch.join(".crates2.json"), CRATES2).unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        env: BTreeMap::from([("CARGO_HOME".into(), ch.to_string_lossy().into_owned())]),
        ..Ctx::default()
    };
    let installed = CargoProvider::new().installed_stub(&["bottom", "cargo-audit"]);
    let bins = CargoProvider::new().bins(&ctx, &installed).unwrap();
    let got: Vec<_> = bins
        .iter()
        .map(|b| (b.package.as_str(), b.bin.as_str()))
        .collect();
    assert_eq!(got, [("bottom", "btm"), ("cargo-audit", "cargo-audit")]);
    assert_eq!(bins[0].path, ch.join("bin/btm"));
}

trait Stub {
    fn installed_stub(&self, names: &[&str]) -> BTreeMap<String, dots_providers::Installed>;
}
impl Stub for CargoProvider {
    fn installed_stub(&self, names: &[&str]) -> BTreeMap<String, dots_providers::Installed> {
        names
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    dots_providers::Installed {
                        version: "1".into(),
                        pinned: false,
                        source: None,
                    },
                )
            })
            .collect()
    }
}

#[test]
fn availability_without_cargo_bootstraps_rustup() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        env: BTreeMap::from([(
            "CARGO_HOME".into(),
            tmp.path().join("nope").to_string_lossy().into_owned(),
        )]),
        ..Ctx::default()
    };
    let a = CargoProvider::new().availability_in(&ctx, &Facts::default());
    // System locations may hold a cargo on a dev machine; only assert the no-cargo shape.
    if !PathBuf::from("/opt/homebrew/bin/cargo").exists()
        && !PathBuf::from("/usr/local/bin/cargo").exists()
        && !PathBuf::from("/usr/bin/cargo").exists()
    {
        assert_eq!(
            a,
            Availability::Bootstrap {
                pkg: "rustup".into()
            }
        );
    }
}

/// Read-only smoke against the real cargo on this machine (skipped when absent).
#[test]
fn real_installed_smoke() {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let ctx = Ctx {
        runner: Arc::new(RealRunner),
        home: home.into(),
        ..Ctx::default()
    };
    if !matches!(
        CargoProvider::new().availability_in(&ctx, &Facts::default()),
        Availability::Ready { .. }
    ) {
        return;
    }
    let m = CargoProvider::for_ctx(&ctx).installed(&ctx).unwrap();
    for (name, i) in &m {
        assert!(!name.is_empty() && !i.version.is_empty());
    }
}

// ------------------------------------------------------- path and git sources

#[test]
fn path_source_installs_from_the_package_dir() {
    let p = CargoProvider::with_bin("/c/cargo");
    let o = op(
        OpKind::Install,
        "herdr-last-tab",
        Some("path=herdr-last-tab;pkg_dir=/repo/packages/herdr"),
        None,
    );
    assert_eq!(
        argv(&p, o.clone()),
        [
            "/c/cargo",
            "install",
            "--locked",
            "--path",
            &std::path::Path::new("/repo/packages/herdr")
                .join("herdr-last-tab")
                .to_string_lossy()
        ]
    );
    let up = Op {
        kind: OpKind::Upgrade,
        ..o.clone()
    };
    assert!(argv(&p, up).contains(&"--force".to_string()));
    let mut with_version = o.clone();
    with_version.version = Some("1.0.0".into());
    assert!(p.plan(&[with_version]).is_err());
    let both = op(OpKind::Install, "x", Some("path=a;git=https://x/y"), None);
    assert!(p.plan(&[both]).is_err());
}

#[test]
#[cfg(unix)]
fn private_git_crate_sets_fetch_with_cli() {
    let p = CargoProvider::with_bin("/c/cargo");
    let o = op(
        OpKind::Install,
        "herdr",
        Some("git=ssh://git@github.com/x/y;git_fetch_with_cli=true"),
        None,
    );
    assert_eq!(
        argv(&p, o.clone()),
        [
            "/usr/bin/env",
            "CARGO_NET_GIT_FETCH_WITH_CLI=true",
            "/c/cargo",
            "install",
            "--locked",
            "--git",
            "ssh://git@github.com/x/y",
            "herdr"
        ]
    );
    // Without the flag nothing is added.
    let plain = op(
        OpKind::Install,
        "herdr",
        Some("git=ssh://git@github.com/x/y"),
        None,
    );
    assert_eq!(argv(&p, plain)[0], "/c/cargo");

    // A bare `cargo` behind the `env` prefix still resolves to the real binary.
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".cargo/bin/cargo");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let a = CargoProvider::new().plan(&[o]).unwrap().remove(0);
    assert!(CargoProvider::new().exec(&ctx, &a).unwrap().success);
    let line = rec.lines().remove(0);
    assert!(
        line.starts_with(&format!(
            "/usr/bin/env CARGO_NET_GIT_FETCH_WITH_CLI=true {} install",
            bin.display()
        )),
        "{line}"
    );
}

/// `git rev-list --count` / `git log -1` answers for the crate dir, changeable between calls.
#[derive(Debug)]
struct FakeGit(Mutex<(u32, String)>);
impl Runner for FakeGit {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let (n, rev) = self.0.lock().unwrap().clone();
        let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        Ok(if args.contains(&"rev-list") {
            Output::ok(&format!("{n}\n"))
        } else if args.contains(&"log") {
            Output::ok(&format!("{rev}\n"))
        } else {
            Output::ok("")
        })
    }
}

#[test]
fn local_crate_goes_outdated_when_its_repo_changes() {
    if !std::path::Path::new("/usr/bin/git").is_file() {
        eprintln!("skipped: no git at a known location");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("repo/herdr");
    let krate = pkg.join("herdr-last-tab");
    std::fs::create_dir_all(&krate).unwrap();
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"herdr-last-tab\"\nversion = \"0.2.0\"\n\n[dependencies]\nversion = \"9\"\n",
    )
    .unwrap();
    let home = tmp.path().join("home");
    let ch = home.join(".cargo");
    std::fs::create_dir_all(ch.join("bin")).unwrap();
    std::fs::write(ch.join("bin/cargo"), "").unwrap();
    let git = Arc::new(FakeGit(Mutex::new((3, "abc1234".into()))));
    let ctx = Ctx {
        runner: git.clone(),
        home: home.clone(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let spec = Spec::new("herdr-last-tab")
        .opt("path", "herdr-last-tab")
        .opt("pkg_dir", &pkg.to_string_lossy());
    let p = CargoProvider::for_ctx(&ctx);

    // The install, as cargo records it in .crates2.json (no repo rev in there).
    let installed_json = format!(
        "{{\"installs\":{{\"herdr-last-tab 0.2.0 (path+file://{})\":{{\"bins\":[\"herdr-last-tab\"]}}}}}}",
        krate.display()
    );
    let a = p
        .plan(&[Op {
            kind: OpKind::Install,
            spec: spec.clone(),
            version: None,
        }])
        .unwrap()
        .remove(0);
    assert!(p.exec(&ctx, &a).unwrap().success);
    std::fs::write(ch.join(".crates2.json"), installed_json).unwrap();

    let have = p.installed(&ctx).unwrap()["herdr-last-tab"].version.clone();
    assert_eq!(have, "0.2.0.3+abc1234");
    let latest = p.latest(&ctx, std::slice::from_ref(&spec)).unwrap();
    assert_eq!(
        latest["herdr-last-tab"].version, have,
        "up to date right after install"
    );

    // A commit changes that directory.
    *git.0.lock().unwrap() = (4, "def5678".into());
    let latest = p.latest(&ctx, &[spec]).unwrap()["herdr-last-tab"]
        .version
        .clone();
    assert_eq!(latest, "0.2.0.4+def5678");
    assert_ne!(latest, have, "status shows outdated");
}

#[test]
fn path_source_without_git_still_reports_the_crate_version() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"1.4.0\"\n",
    )
    .unwrap();
    let ctx = Ctx {
        runner: Arc::new(RecordingRunner::new()),
        home: tmp.path().join("home"),
        ..Ctx::default()
    };
    let spec = Spec::new("x").opt("path", &tmp.path().to_string_lossy());
    let v = CargoProvider::new().latest(&ctx, &[spec]).unwrap()["x"]
        .version
        .clone();
    assert!(v.starts_with("1.4.0"), "{v}");
}

/// A `--locked` install that fails to compile hints at `locked = false`; a plain failure
/// (network) does not; without `--locked` there is no hint to give.
#[test]
fn locked_compile_failure_hints_locked_false() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".cargo/bin/cargo");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let run = |opts: &str, stderr: &str| {
        let rec = Arc::new(RecordingRunner::new());
        rec.on(&[], Output::fail(101, stderr));
        let ctx = Ctx {
            runner: rec,
            home: tmp.path().into(),
            log_dir: tmp.path().join("logs"),
            ..Ctx::default()
        };
        let mut s = Spec::new("igrep");
        for pair in opts.split(';') {
            if let Some((k, v)) = pair.split_once('=') {
                s = s.opt(k, v);
            }
        }
        let p = CargoProvider::new();
        let a = p
            .plan(&[Op {
                kind: OpKind::Install,
                spec: s,
                version: None,
            }])
            .unwrap()
            .remove(0);
        p.exec(&ctx, &a)
    };
    let compile =
        "error[E0658]: use of unstable library feature\nerror: could not compile `igrep`\n";
    let err = run("", compile).unwrap_err().to_string();
    assert!(err.contains("locked = false"), "{err}");
    assert!(!run("", "error: failed to fetch index\n").unwrap().success);
    let out = run("locked=false", compile).unwrap();
    assert!(!out.success, "no hint and no error without --locked");
}

// ------------------------------------------------------- install source mismatch

const FORK: &str = "https://github.com/ofirgall/herdr";
const FORK_REV: &str = "6644000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A cargo home whose `.crates2.json` has one install of `herdr` from `source`.
fn home_with_install(source: &str) -> (tempfile::TempDir, Ctx) {
    let tmp = tempfile::tempdir().unwrap();
    let ch = tmp.path().join(".cargo");
    std::fs::create_dir_all(&ch).unwrap();
    std::fs::write(
        ch.join(".crates2.json"),
        format!(
            "{{\"installs\":{{\"herdr 0.8.2 ({source})\":{{\"bins\":[\"herdr\"],\"features\":[]}}}}}}"
        ),
    )
    .unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        ..Ctx::default()
    };
    (tmp, ctx)
}

fn stale(ctx: &Ctx, spec: &Spec) -> Option<String> {
    CargoProvider::with_bin("/c/cargo").stale_reason(ctx, spec)
}

fn git_spec(url: &str, rev: Option<&str>) -> Spec {
    let s = Spec::new("herdr").opt("git", url);
    match rev {
        Some(r) => s.opt("rev", r),
        None => s,
    }
}

#[test]
fn a_local_path_build_is_stale_when_the_repo_wants_a_git_fork() {
    let (_t, ctx) = home_with_install("path+file:///Users/x/workspace/personal/herdr");
    let note = stale(&ctx, &git_spec(FORK, Some(FORK_REV))).expect("stale");
    assert_eq!(
        note,
        "installed from path+file:///Users/x/workspace/personal/herdr, \
         repo wants git ofirgall/herdr@6644000"
    );
    // Without a pinned rev the note names just the repo.
    let note = stale(&ctx, &git_spec(FORK, None)).unwrap();
    assert!(note.ends_with("repo wants git ofirgall/herdr"), "{note}");
}

#[test]
fn a_registry_install_is_stale_when_the_repo_wants_git_or_a_path() {
    let (_t, ctx) = home_with_install("registry+https://github.com/rust-lang/crates.io-index");
    assert!(stale(&ctx, &git_spec(FORK, None)).is_some());
    assert!(stale(&ctx, &Spec::new("herdr").opt("path", "/src/herdr")).is_some());
    // No git or path declared: a registry crate is never stale on this account.
    assert_eq!(stale(&ctx, &Spec::new("herdr")), None);
}

#[test]
fn the_same_git_repo_is_not_stale_whatever_its_spelling() {
    let (_t, ctx) = home_with_install(&format!("git+{FORK}?rev=6644000#{FORK_REV}"));
    for url in [
        FORK,
        "https://github.com/ofirgall/herdr.git",
        "https://github.com/OfirGall/Herdr/",
        "git@github.com:ofirgall/herdr.git",
        "ssh://git@github.com/ofirgall/herdr",
    ] {
        assert_eq!(stale(&ctx, &git_spec(url, Some("6644000"))), None, "{url}");
        assert_eq!(stale(&ctx, &git_spec(url, None)), None, "{url}");
    }
    // The ssh spelling on the installed side matches an https declaration, too.
    let (_t, ctx) = home_with_install(&format!(
        "git+ssh://git@github.com/ofirgall/herdr.git#{FORK_REV}"
    ));
    assert_eq!(stale(&ctx, &git_spec(FORK, Some(FORK_REV))), None);
}

#[test]
fn a_different_git_repo_or_pinned_rev_is_stale() {
    let (_t, ctx) = home_with_install(&format!("git+{FORK}#{FORK_REV}"));
    // Another repo.
    let note = stale(&ctx, &git_spec("https://github.com/upstream/herdr", None)).unwrap();
    assert!(note.contains("repo wants git upstream/herdr"), "{note}");
    // A different pinned rev; an unpinned spec accepts any rev.
    assert!(stale(&ctx, &git_spec(FORK, Some("1234567"))).is_some());
    assert_eq!(stale(&ctx, &git_spec(FORK, None)), None);
    assert_eq!(stale(&ctx, &git_spec(FORK, Some("6644000"))), None);
}

#[test]
fn a_path_install_from_another_directory_or_kind_is_stale() {
    let (_t, ctx) = home_with_install("path+file:///src/herdr");
    let want = |p: &str| Spec::new("herdr").opt("path", p);
    assert_eq!(stale(&ctx, &want("/src/herdr")), None);
    assert_eq!(stale(&ctx, &want("/src/herdr/")), None);
    assert!(stale(&ctx, &want("/other/herdr")).is_some());
    // Relative to the package directory, like the plan resolves it.
    assert_eq!(stale(&ctx, &want("herdr").opt("pkg_dir", "/src")), None);
    let (_t, ctx) = home_with_install(&format!("git+{FORK}#{FORK_REV}"));
    assert!(stale(&ctx, &want("/src/herdr")).is_some());
}

#[test]
fn the_source_falls_back_to_crates_toml_and_unknown_is_not_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let ch = tmp.path().join(".cargo");
    std::fs::create_dir_all(&ch).unwrap();
    std::fs::write(
        ch.join(".crates.toml"),
        "[v1]\n\"herdr 0.8.2 (path+file:///Users/x/herdr)\" = [\"herdr\"]\n",
    )
    .unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        ..Ctx::default()
    };
    assert!(stale(&ctx, &git_spec(FORK, None)).is_some());
    // Another crate's record says nothing about this one.
    let other = Spec::new("other").opt("git", FORK);
    assert_eq!(stale(&ctx, &other), None);
    // No cargo records at all: nothing to compare with.
    let empty = Ctx {
        home: tempfile::tempdir().unwrap().path().into(),
        ..Ctx::default()
    };
    assert_eq!(stale(&empty, &git_spec(FORK, None)), None);
}

#[test]
fn a_git_upgrade_replaces_the_install_with_force() {
    let p = CargoProvider::with_bin("/c/cargo");
    let spec = "git=https://github.com/ofirgall/herdr;rev=6644000";
    let up = argv(&p, op(OpKind::Upgrade, "herdr", Some(spec), None));
    assert_eq!(
        up,
        [
            "/c/cargo",
            "install",
            "--locked",
            "--force",
            "--git",
            "https://github.com/ofirgall/herdr",
            "--rev",
            "6644000",
            "herdr"
        ]
    );
    let first = argv(&p, op(OpKind::Install, "herdr", Some(spec), None));
    assert!(!first.contains(&"--force".to_string()));
}

// ------------------------------------------------------- git fetch with the CLI

fn ctx_in(tmp: &std::path::Path, rec: Arc<RecordingRunner>) -> Ctx {
    let bin = tmp.join(".cargo/bin/cargo");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    Ctx {
        runner: rec,
        home: tmp.into(),
        log_dir: tmp.join("logs"),
        ..Ctx::default()
    }
}

#[test]
fn cargo_commands_fetch_git_with_the_cli_unless_the_user_says_otherwise() {
    let tmp = tempfile::tempdir().unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = ctx_in(tmp.path(), rec.clone());
    let p = CargoProvider::for_ctx(&ctx);
    let a = p
        .plan(&[op(
            OpKind::Install,
            "herdr",
            Some("git=https://github.com/ofirgall/herdr"),
            None,
        )])
        .unwrap()
        .remove(0);
    assert!(p.exec(&ctx, &a).unwrap().success);
    let install = rec.commands().remove(0);
    assert_eq!(
        install
            .env
            .get("CARGO_NET_GIT_FETCH_WITH_CLI")
            .map(String::as_str),
        Some("true"),
        "{install:?}"
    );

    // The user's own setting wins: dots adds nothing.
    let rec = Arc::new(RecordingRunner::new());
    let mut ctx = ctx_in(tmp.path(), rec.clone());
    ctx.env
        .insert("CARGO_NET_GIT_FETCH_WITH_CLI".into(), "false".into());
    assert!(p.exec(&ctx, &a).unwrap().success);
    let install = rec.commands().remove(0);
    assert_eq!(
        install
            .env
            .get("CARGO_NET_GIT_FETCH_WITH_CLI")
            .map(String::as_str),
        Some("false"),
        "{install:?}"
    );
}
