use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dots_facts::Facts;
use dots_providers::uv::UvProvider;
use dots_providers::{
    Availability, Cmd, Ctx, Installed, Op, OpKind, Output, Provider, RealRunner, Result, Runner,
    Spec,
};
use dots_testkit::{ConformanceFixture, RecordingRunner, conformance_test};

const TOOL_LIST: &str = include_str!("fixtures/uv/tool-list.txt");
const RECEIPT: &str = include_str!("fixtures/uv/uv-receipt.toml");

/// A tiny in-memory uv: `tool list` and `tool install name==ver`.
#[derive(Debug, Default)]
struct FakeUv(Mutex<BTreeMap<String, String>>);

impl Runner for FakeUv {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut s = self.0.lock().unwrap();
        match cmd.args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["tool", "list", ..] => {
                let text: String = s
                    .iter()
                    .map(|(n, v)| format!("{n} v{v}\n- {n}\n"))
                    .collect();
                Ok(Output::ok(&text))
            }
            ["tool", "install", req] => {
                let (n, v) = req.split_once("==").unwrap_or((req, "9.9.9"));
                s.insert(n.to_string(), v.to_string());
                Ok(Output::ok(""))
            }
            _ => Ok(Output::fail(2, "unexpected")),
        }
    }
}

conformance_test!(uv_conforms, {
    let fake = FakeUv::default();
    fake.0
        .lock()
        .unwrap()
        .insert("present".into(), "1.0.0".into());
    let fx = ConformanceFixture::new("present", "absent")
        .exact_version("2.0.0")
        .backend(Arc::new(fake));
    let bin = fx.sandbox_dir().join("home/.local/bin/uv");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    (UvProvider::for_ctx(&fx.ctx()), fx)
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

fn argv(p: &UvProvider, o: Op) -> Vec<String> {
    p.plan(&[o]).unwrap().remove(0).argv
}

#[test]
fn tool_plan() {
    let p = UvProvider::with_bin("/u/uv");
    assert_eq!(
        argv(&p, op(OpKind::Install, "ruff", None, Some("0.4.0"))),
        ["/u/uv", "tool", "install", "ruff==0.4.0"]
    );
    assert_eq!(
        argv(&p, op(OpKind::Remove, "ruff", None, None)),
        ["/u/uv", "tool", "uninstall", "ruff"]
    );
}

/// Spec scenario: `[providers.uv] kind = "lib", python = "system"` installs with uv into the
/// system Python, and that is shown in the plan.
#[test]
fn library_mode_installs_into_system_python() {
    let p = UvProvider::with_bin("/u/uv");
    let actions = p
        .plan(&[op(
            OpKind::Install,
            "libtmux",
            Some("kind=lib;python=system"),
            Some("0.37.0"),
        )])
        .unwrap();
    assert_eq!(
        actions[0].argv,
        ["/u/uv", "pip", "install", "--system", "libtmux==0.37.0"]
    );
    let other = argv(
        &p,
        op(
            OpKind::Install,
            "libtmux",
            Some("kind=lib;python=/opt/py/bin/python"),
            None,
        ),
    );
    assert_eq!(
        other,
        [
            "/u/uv",
            "pip",
            "install",
            "--python",
            "/opt/py/bin/python",
            "libtmux"
        ]
    );
    assert!(
        p.plan(&[op(OpKind::Install, "x", Some("kind=venv"), None)])
            .is_err()
    );
}

#[test]
fn installed_parses_fixture_with_one_call() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".local/bin/uv");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &[bin.to_str().unwrap(), "tool", "list"],
        Output::ok(TOOL_LIST),
    );
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        ..Ctx::default()
    };
    let m = UvProvider::for_ctx(&ctx).installed(&ctx).unwrap();
    assert_eq!(rec.count(), 1);
    assert_eq!(m["claude-swap"].version, "0.29.0");
    assert_eq!(
        m["agents-status"].source.as_deref(),
        Some("file:///home/me/agents-status/core")
    );
}

#[test]
fn bins_from_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tools/claude-swap");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("uv-receipt.toml"), RECEIPT).unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        env: BTreeMap::from([(
            "UV_TOOL_DIR".into(),
            tmp.path().join("tools").to_string_lossy().into_owned(),
        )]),
        ..Ctx::default()
    };
    let installed = BTreeMap::from([(
        "claude-swap".to_string(),
        Installed {
            version: "0.29.0".into(),
            pinned: false,
            source: None,
        },
    )]);
    let bins = UvProvider::new().bins(&ctx, &installed).unwrap();
    let got: Vec<_> = bins.iter().map(|b| b.bin.as_str()).collect();
    assert_eq!(got, ["claude-swap", "cswap"]);
    assert_eq!(bins[1].path.to_str(), Some("/home/me/.local/bin/cswap"));
}

#[test]
fn availability_without_uv_bootstraps_uv() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        ..Ctx::default()
    };
    let a = UvProvider::new().availability_in(&ctx, &Facts::default());
    if !["/opt/homebrew/bin/uv", "/usr/local/bin/uv", "/usr/bin/uv"]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
    {
        assert_eq!(a, Availability::Bootstrap { pkg: "uv".into() });
    }
}

/// Read-only smoke against the real uv on this machine (skipped when absent).
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
        UvProvider::new().availability_in(&ctx, &Facts::default()),
        Availability::Ready { .. }
    ) {
        return;
    }
    let m = UvProvider::for_ctx(&ctx).installed(&ctx).unwrap();
    for (name, i) in &m {
        assert!(!name.is_empty() && !i.version.is_empty());
    }
}

#[test]
fn installed_covers_lib_packages_with_one_call_per_target() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".local/bin/uv");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let uv = bin.to_str().unwrap();
    let rec = Arc::new(RecordingRunner::new());
    rec.on(&[uv, "tool", "list"], Output::ok(TOOL_LIST));
    rec.on(
        &[uv, "pip", "list", "--system", "--format", "json"],
        Output::ok(r#"[{"name":"Lib_Tmux","version":"0.37.0"},{"name":"other","version":"1"}]"#),
    );
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        ..Ctx::default()
    };
    let p = UvProvider::for_ctx(&ctx)
        .lib("lib-tmux", "system")
        .lib("libyaml", "system");
    let m = p.installed(&ctx).unwrap();
    assert_eq!(rec.count(), 2, "{:?}", rec.lines());
    assert_eq!(m["lib-tmux"].version, "0.37.0");
    assert!(!m.contains_key("libyaml"));
    assert!(m.contains_key("claude-swap"));
}

/// A plan made with `new()` (bare `uv`) still executes through the resolved binary.
#[test]
fn exec_resolves_bare_program() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".local/bin/uv");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let p = UvProvider::new();
    let a = p
        .plan(&[op(OpKind::Install, "ruff", None, None)])
        .unwrap()
        .remove(0);
    assert_eq!(a.argv[0], "uv");
    assert!(p.exec(&ctx, &a).unwrap().success);
    assert_eq!(rec.commands()[0].program, bin);
}
