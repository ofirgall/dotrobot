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
