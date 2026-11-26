//! Tests for user-defined declarative providers.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dots_facts::Facts;
use dots_providers::custom::{CustomDef, CustomProvider, Parse};
use dots_providers::{
    Availability, Cmd, Ctx, Op, OpKind, Output, Provider, Result, Runner, Spec, Support,
};
use dots_testkit::{ConformanceFixture, RecordingRunner, conformance_test};
use serde_json::json;

fn fixture(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/custom")
            .join(name),
    )
    .unwrap()
}

fn go_table() -> serde_json::Value {
    json!({
        "available": "command -v go",
        "bootstrap": "go",
        "installed": { "cmd": "ls ~/go/bin", "parse": "lines" },
        "version": "go version -m ~/go/bin/{bin} | awk '/^\\tmod/{print $3}'",
        "install": "go install {source}@{version|latest}",
        "batch": false
    })
}

fn go() -> CustomProvider {
    let def = CustomDef::from_json("go", &go_table()).unwrap();
    CustomProvider::new(def, &Facts::default())
}

fn install_op(name: &str, source: Option<&str>, version: Option<&str>) -> Op {
    Op {
        kind: OpKind::Install,
        spec: Spec {
            source: source.map(String::from),
            ..Spec::new(name)
        },
        version: version.map(String::from),
    }
}

#[test]
fn go_provider_plan_shows_the_exact_argv() {
    let a = go()
        .plan(&[install_op("y", Some("github.com/x/y"), None)])
        .unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].argv, ["go", "install", "github.com/x/y@latest"]);
    assert!(!a[0].needs_root);
}

#[test]
fn exact_version_fills_the_placeholder() {
    let a = go()
        .plan(&[install_op("y", Some("github.com/x/y"), Some("v1.2.0"))])
        .unwrap();
    assert_eq!(a[0].argv, ["go", "install", "github.com/x/y@v1.2.0"]);
    assert_eq!(go().caps().exact_version, Support::Yes);
}

#[test]
fn name_placeholder_and_missing_version() {
    let mut t = go_table();
    t["install"] = json!("go install {name}@{version}");
    let p = CustomProvider::new(CustomDef::from_json("go", &t).unwrap(), &Facts::default());
    let a = p.plan(&[install_op("tool", None, Some("v1"))]).unwrap();
    assert_eq!(a[0].argv, ["go", "install", "tool@v1"]);
    assert!(p.plan(&[install_op("tool", None, None)]).is_err());
}

#[test]
fn fact_placeholders_and_shell_syntax() {
    let mut t = go_table();
    t["install"] = json!("curl -L https://x/{os}-{arch}/{name} | tar x");
    let facts = Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        ..Facts::default()
    };
    let p = CustomProvider::new(CustomDef::from_json("go", &t).unwrap(), &facts);
    let a = p.plan(&[install_op("kv", None, None)]).unwrap();
    assert_eq!(
        a[0].argv,
        ["/bin/sh", "-c", "curl -L https://x/linux-x86_64/kv | tar x"]
    );
    assert_eq!(p.caps().exact_version, Support::No);
    assert!(p.plan(&[install_op("kv", None, Some("1"))]).is_err());
}

#[test]
fn batch_installs_all_ops_in_one_action() {
    let mut t = go_table();
    t["install"] = json!("gem install {names}");
    t["batch"] = json!(true);
    let p = CustomProvider::new(CustomDef::from_json("gem", &t).unwrap(), &Facts::default());
    assert!(p.caps().batch_install);
    let a = p
        .plan(&[
            install_op("rake", None, None),
            install_op("bundler", None, None),
        ])
        .unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].argv, ["gem", "install", "rake", "bundler"]);
    assert_eq!(a[0].ops.len(), 2);
    t["install"] = json!("gem install {name}");
    assert!(CustomDef::from_json("gem", &t).is_err());
}

#[test]
fn remove_and_pin_are_refused() {
    let p = go();
    let rm = Op {
        kind: OpKind::Remove,
        ..install_op("y", None, None)
    };
    assert!(
        p.plan(&[rm])
            .unwrap_err()
            .to_string()
            .contains("cannot uninstall")
    );
    let pin = Op {
        kind: OpKind::Pin,
        ..install_op("y", None, None)
    };
    assert!(p.plan(&[pin]).is_err());
}

#[test]
fn declaration_is_validated() {
    let bad = |v: serde_json::Value| CustomDef::from_json("x", &v).unwrap_err().to_string();
    assert!(bad(json!({"installed": "ls"})).contains("`install` is required"));
    assert!(bad(json!({"install": "x"})).contains("`installed` is required"));
    assert!(
        bad(json!({"install": "x", "installed": "ls", "nope": 1})).contains("unknown key `nope`")
    );
    assert!(
        bad(json!({"install": "x", "installed": {"cmd": "ls", "parse": "xml"}}))
            .contains("unknown parse mode")
    );
    assert!(
        bad(json!({"install": "x", "installed": {"cmd": "ls", "parse": "regex:("}}))
            .contains("invalid parse regex")
    );
    let d = CustomDef::from_json("x", &json!({"install": "x", "installed": "ls"})).unwrap();
    assert_eq!(d.parse, Parse::Lines);
}

fn ctx_with(r: &Arc<RecordingRunner>) -> Ctx {
    Ctx {
        runner: r.clone(),
        ..Ctx::default()
    }
}

#[test]
fn availability_ready_bootstrap_or_unsupported() {
    let facts = Facts::default();
    let r = Arc::new(RecordingRunner::new());
    r.on(
        &["/bin/sh", "-c", "command -v go"],
        Output::ok("/usr/local/go/bin/go\n"),
    );
    assert_eq!(
        go().availability_in(&ctx_with(&r), &facts),
        Availability::Ready {
            bin_path: "/usr/local/go/bin/go".into()
        }
    );
    let r = Arc::new(RecordingRunner::new());
    r.on(&["/bin/sh", "-c", "command -v go"], Output::fail(1, ""));
    assert_eq!(
        go().availability_in(&ctx_with(&r), &facts),
        Availability::Bootstrap { pkg: "go".into() }
    );
    let mut t = go_table();
    t.as_object_mut().unwrap().remove("bootstrap");
    let p = CustomProvider::new(CustomDef::from_json("go", &t).unwrap(), &facts);
    assert_eq!(
        p.availability_in(&ctx_with(&r), &facts),
        Availability::Unsupported
    );
}

fn provider_with_parse(
    parse: &str,
    out: &str,
) -> std::collections::BTreeMap<String, dots_providers::Installed> {
    let t = json!({"install": "x {name}", "installed": {"cmd": "list", "parse": parse}});
    let p = CustomProvider::new(CustomDef::from_json("x", &t).unwrap(), &Facts::default());
    let r = Arc::new(RecordingRunner::new());
    r.on(&["/bin/sh", "-c", "list"], Output::ok(out));
    p.installed(&ctx_with(&r)).unwrap()
}

#[test]
fn parse_lines_with_and_without_versions() {
    let m = provider_with_parse("lines", "alpha 1.0\nbeta\n\n");
    assert_eq!(m["alpha"].version, "1.0");
    assert_eq!(m["beta"].version, "unknown");
}

#[test]
fn parse_json_object_path() {
    let m = provider_with_parse("json:dependencies", &fixture("npm-list.json"));
    assert_eq!(m["typescript"].version, "5.4.2");
    assert_eq!(m["eslint"].version, "9.0.1");
}

#[test]
fn parse_json_array_of_objects_and_strings() {
    let m = provider_with_parse(
        "json:pkgs",
        r#"{"pkgs": [{"name": "a", "version": "1"}, "b"]}"#,
    );
    assert_eq!(
        (m["a"].version.as_str(), m["b"].version.as_str()),
        ("1", "unknown")
    );
}

#[test]
fn parse_regex_named_and_numbered_groups() {
    let m = provider_with_parse(
        r"regex:^(?P<name>\S+) \((?P<version>[^)]+)\)$",
        &fixture("gem-list.txt"),
    );
    assert_eq!(m["rake"].version, "13.1.0");
    assert_eq!(m["bundler"].version, "2.5.6");
    let m = provider_with_parse(r"regex:^(\w+) v(\S+)", "foo v1.2\nbar v3\nnoise\n");
    assert_eq!(
        (m["foo"].version.as_str(), m["bar"].version.as_str()),
        ("1.2", "3")
    );
    assert_eq!(m.len(), 2);
}

#[test]
fn version_command_fills_missing_versions() {
    let r = Arc::new(RecordingRunner::new());
    r.on(&["/bin/sh", "-c", "ls ~/go/bin"], Output::ok("y\n"));
    r.on(
        &[
            "/bin/sh",
            "-c",
            "go version -m ~/go/bin/y | awk '/^\\tmod/{print $3}'",
        ],
        Output::ok("v0.4.1\n"),
    );
    let m = go().installed(&ctx_with(&r)).unwrap();
    assert_eq!(m["y"].version, "v0.4.1");
}

/// An in-memory `go`: `ls` lists what is installed, `go install` adds a package.
#[derive(Debug, Default)]
struct FakeGo {
    pkgs: Mutex<Vec<(String, String)>>,
}

impl Runner for FakeGo {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let line = cmd.line();
        let mut pkgs = self.pkgs.lock().unwrap();
        if line.contains("command -v go") {
            return Ok(Output::ok("/usr/local/go/bin/go\n"));
        }
        if line.contains("ls ~/go/bin") {
            let s: String = pkgs.iter().map(|(n, v)| format!("{n} {v}\n")).collect();
            return Ok(Output::ok(&s));
        }
        if cmd.args.first().map(String::as_str) == Some("install") {
            let (name, ver) = cmd.args[1].split_once('@').unwrap();
            let ver = if ver == "latest" { "9.9.9" } else { ver };
            pkgs.push((name.to_string(), ver.to_string()));
            return Ok(Output::ok("installed\n"));
        }
        Ok(Output::fail(127, "unexpected command"))
    }
}

conformance_test!(custom_go_provider_conforms, {
    let backend = FakeGo::default();
    backend
        .pkgs
        .lock()
        .unwrap()
        .push(("alpha".to_string(), "1.0.0".to_string()));
    let fixture = ConformanceFixture::new("alpha", "beta")
        .exact_version("1.2.3")
        .backend(Arc::new(backend));
    (go(), fixture)
});

/// The resolver calls `availability(facts)` with no context. A provider given a probe
/// context runs its `available` command (once): `Ready` when it passes, `Bootstrap` when it
/// fails and the tool comes from another package of the run, and `installed()` is empty (not
/// an error) until then.
#[test]
fn availability_without_a_context_probes_available_once() {
    let facts = Facts::default();
    let r = Arc::new(RecordingRunner::new());
    r.on(&["/bin/sh", "-c", "command -v go"], Output::fail(1, ""));
    let p = CustomProvider::new(CustomDef::from_json("go", &go_table()).unwrap(), &facts)
        .with_probe(ctx_with(&r));
    for _ in 0..3 {
        assert_eq!(
            p.availability(&facts),
            Availability::Bootstrap { pkg: "go".into() },
            "the tool is installed by the `go` package of this run"
        );
    }
    assert_eq!(
        r.lines()
            .iter()
            .filter(|l| l.ends_with("command -v go"))
            .count(),
        1,
        "probed once: {:?}",
        r.lines()
    );
    // Not there yet: nothing installed, and the listing command is never run.
    assert!(p.installed(&ctx_with(&r)).unwrap().is_empty());
    assert!(!r.lines().iter().any(|l| l.contains("ls ~/go/bin")));

    let r = Arc::new(RecordingRunner::new());
    r.on(
        &["/bin/sh", "-c", "command -v go"],
        Output::ok("/usr/local/go/bin/go\n"),
    );
    let p = CustomProvider::new(CustomDef::from_json("go", &go_table()).unwrap(), &facts)
        .with_probe(ctx_with(&r));
    assert_eq!(
        p.availability(&facts),
        Availability::Ready {
            bin_path: "/usr/local/go/bin/go".into()
        }
    );
    // Without a probe context nothing is guessed: only the facts count.
    assert_eq!(go().availability(&facts), Availability::Unsupported);
}
