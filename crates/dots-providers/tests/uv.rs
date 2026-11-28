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

/// `dir` joined with `leaf` by the platform's rules, as the provider builds paths.
fn joined(dir: &str, leaf: &str) -> String {
    std::path::Path::new(dir)
        .join(leaf)
        .to_string_lossy()
        .into_owned()
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
    let err = p
        .plan(&[op(OpKind::Install, "x", Some("kind=conda"), None)])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`conda`") && err.contains("`tool`, `lib` or `venv`"),
        "{err}"
    );
}

/// ARCHITECTURE 5.2: `kind = "venv"` + `requirements` creates `~/.local/share/dots/venvs/<name>`
/// and installs the requirements into it (it used to be refused as "not supported yet").
#[test]
#[cfg(unix)]
fn venv_kind_creates_the_venv_and_installs_requirements() {
    let p = UvProvider::with_bin("/u/uv").with_venv_root("/h/venvs");
    let spec = "kind=venv;requirements=requirements.txt;pkg_dir=/repo/packages/scripts";
    let actions = p
        .plan(&[op(OpKind::Install, "scripts", Some(spec), None)])
        .unwrap();
    assert_eq!(actions.len(), 2);
    assert!(actions[0].ops.is_empty(), "venv creation is a prep action");
    assert_eq!(
        actions[0].argv,
        [
            "/u/uv",
            "venv",
            "--allow-existing",
            &joined("/h/venvs", "scripts")
        ]
    );
    assert_eq!(actions[1].ops.len(), 1);
    assert_eq!(
        actions[1].argv,
        [
            "/u/uv",
            "pip",
            "install",
            "--python",
            "/h/venvs/scripts/bin/python",
            "-r",
            "/repo/packages/scripts/requirements.txt"
        ]
    );
    let up = p
        .plan(&[op(OpKind::Upgrade, "scripts", Some(spec), None)])
        .unwrap();
    assert!(up[1].argv.contains(&"--upgrade".to_string()));
    assert_eq!(
        argv(&p, op(OpKind::Remove, "scripts", Some("kind=venv"), None)),
        ["/bin/rm", "-rf", "/h/venvs/scripts"]
    );
}

#[test]
fn a_venv_directory_counts_as_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".local/bin/uv");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let venv = tmp.path().join(".local/share/dots/venvs/scripts");
    std::fs::create_dir_all(&venv).unwrap();
    std::fs::write(venv.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
    std::fs::create_dir_all(tmp.path().join(".local/share/dots/venvs/half")).unwrap();
    let rec = Arc::new(RecordingRunner::new());
    rec.on(&[bin.to_str().unwrap(), "tool", "list"], Output::ok(""));
    let ctx = Ctx {
        runner: rec,
        home: tmp.path().into(),
        ..Ctx::default()
    };
    let m = UvProvider::for_ctx(&ctx).installed(&ctx).unwrap();
    assert!(m.contains_key("scripts"));
    assert!(!m.contains_key("half"), "no pyvenv.cfg, not a venv");
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

#[test]
fn path_and_git_sources() {
    let p = UvProvider::with_bin("/u/uv");
    let path = op(
        OpKind::Install,
        "mytool",
        Some("path=mytool;pkg_dir=/repo/packages/x"),
        None,
    );
    assert_eq!(
        argv(&p, path.clone()),
        [
            "/u/uv",
            "tool",
            "install",
            &joined("/repo/packages/x", "mytool")
        ]
    );
    let up = Op {
        kind: OpKind::Upgrade,
        ..path
    };
    assert_eq!(
        argv(&p, up),
        [
            "/u/uv",
            "tool",
            "install",
            "--force",
            &joined("/repo/packages/x", "mytool")
        ]
    );
    let git = op(
        OpKind::Install,
        "mytool",
        Some("git=git+ssh://git@github.com/x/y"),
        None,
    );
    assert_eq!(
        argv(&p, git),
        ["/u/uv", "tool", "install", "git+ssh://git@github.com/x/y"]
    );
    let rev = op(
        OpKind::Install,
        "mytool",
        Some("git=ssh://git@github.com/x/y;rev=abc;git_fetch_with_cli=true"),
        None,
    );
    assert_eq!(
        argv(&p, rev),
        [
            "/u/uv",
            "tool",
            "install",
            "git+ssh://git@github.com/x/y@abc"
        ]
    );
    assert!(
        p.plan(&[op(OpKind::Install, "t", Some("path=a;git=b"), None)])
            .is_err()
    );
    assert!(
        p.plan(&[op(OpKind::Install, "t", Some("git=b"), Some("1.0"))])
            .is_err()
    );
}

#[test]
fn local_tool_goes_outdated_when_its_repo_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let krate = tmp.path().join("mytool");
    std::fs::create_dir_all(&krate).unwrap();
    std::fs::write(
        krate.join("pyproject.toml"),
        "[project]\nname = \"mytool\"\nversion = \"0.5.1\"\n",
    )
    .unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::fs::write(home.join(".local/bin/uv"), "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let git = host_git();
    rec.on(&[git.as_str()], Output::ok("2\n"));
    rec.on(
        &[home.join(".local/bin/uv").to_str().unwrap(), "tool", "list"],
        Output::ok("mytool v0.5.1\n- mytool\n"),
    );
    let ctx = Ctx {
        runner: rec.clone(),
        home: home.clone(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let receipt_dir = home.join(".local/share/uv/tools/mytool");
    std::fs::create_dir_all(&receipt_dir).unwrap();
    std::fs::write(
        receipt_dir.join("uv-receipt.toml"),
        format!(
            "[tool]\nrequirements = [{{ name = \"mytool\", directory = \"{}\" }}]\n",
            krate.display()
        ),
    )
    .unwrap();
    let spec = Spec::new("mytool").opt("path", &krate.to_string_lossy());
    let p = UvProvider::for_ctx(&ctx);
    let a = p
        .plan(&[Op {
            kind: OpKind::Install,
            spec: spec.clone(),
            version: None,
        }])
        .unwrap()
        .remove(0);
    assert!(p.exec(&ctx, &a).unwrap().success);
    let have = p.installed(&ctx).unwrap()["mytool"].version.clone();
    let latest = p.latest(&ctx, std::slice::from_ref(&spec)).unwrap()["mytool"]
        .version
        .clone();
    if std::path::Path::new(&git).is_file() {
        assert!(have.starts_with("0.5.1.2+"), "{have}");
        assert_eq!(have, latest);
        // The tree gains a commit.
        rec.clear();
        let rec2 = Arc::new(RecordingRunner::new());
        rec2.on(&[git.as_str()], Output::ok("3\n"));
        let ctx2 = Ctx {
            runner: rec2,
            ..ctx
        };
        let newer = p.latest(&ctx2, &[spec]).unwrap()["mytool"].version.clone();
        assert!(newer.starts_with("0.5.1.3+"), "{newer}");
        assert_ne!(newer, have);
    }
}

/// A stale path-rev sidecar entry must not override the version of a tool that has since
/// been installed from a registry.
#[test]
fn stale_path_rev_does_not_override_a_registry_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let uv = home.join(".local/bin/uv");
    std::fs::create_dir_all(uv.parent().unwrap()).unwrap();
    std::fs::write(&uv, "").unwrap();
    let tools = home.join(".local/share/uv/tools");
    for (name, req) in [
        ("regtool", "{ name = \"regtool\" }".to_string()),
        (
            "pathtool",
            "{ name = \"pathtool\", directory = \"/src/pathtool\" }".to_string(),
        ),
    ] {
        std::fs::create_dir_all(tools.join(name)).unwrap();
        std::fs::write(
            tools.join(name).join("uv-receipt.toml"),
            format!("[tool]\nrequirements = [{req}]\n"),
        )
        .unwrap();
    }
    std::fs::write(
        home.join(".local/share/uv/dots-path-revs.json"),
        r#"{"regtool":"0.5.1.2+abc","pathtool":"0.5.1.2+abc"}"#,
    )
    .unwrap();
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &[uv.to_str().unwrap(), "tool", "list"],
        Output::ok("regtool v2.0.0\n- regtool\npathtool v0.5.1\n- pathtool\n"),
    );
    let ctx = Ctx {
        runner: rec,
        home,
        ..Ctx::default()
    };
    let m = UvProvider::for_ctx(&ctx).installed(&ctx).unwrap();
    assert_eq!(
        m["regtool"].version, "2.0.0",
        "registry tool keeps its real version"
    );
    assert_eq!(
        m["pathtool"].version, "0.5.1.2+abc",
        "path tool uses the sidecar"
    );
}

/// A context whose runner reports a system python with the given stdlib dir, and uv installed.
fn lib_ctx(stdlib: &std::path::Path) -> (tempfile::TempDir, Arc<RecordingRunner>, Ctx) {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join(".local/bin/uv");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &["python3", "-c"],
        Output::ok(&format!(
            "{}\n{}/site-packages\n",
            stdlib.display(),
            stdlib.display()
        )),
    );
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    (tmp, rec, ctx)
}

/// PEP 668 (Ubuntu 24.04): a system lib on an externally managed python fails clearly, runs
/// nothing, and points at `kind = "venv"` and the opt-in.
#[test]
fn system_lib_on_externally_managed_python_is_refused() {
    let std_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        std_dir.path().join("EXTERNALLY-MANAGED"),
        "[externally-managed]\n",
    )
    .unwrap();
    let (_t, rec, ctx) = lib_ctx(std_dir.path());
    let p = UvProvider::new();
    let a = p
        .plan(&[op(
            OpKind::Install,
            "libtmux",
            Some("kind=lib;python=system"),
            None,
        )])
        .unwrap()
        .remove(0);
    assert!(!a.needs_root);
    let err = p.exec(&ctx, &a).unwrap_err().to_string();
    assert!(
        err.contains("externally managed")
            && err.contains("kind = \"venv\"")
            && err.contains("break_system_packages = true"),
        "{err}"
    );
    assert!(
        rec.lines().iter().all(|l| !l.contains("pip install")),
        "{:?}",
        rec.lines()
    );
}

/// A python without the marker installs as before.
#[test]
fn system_lib_on_unmanaged_python_installs() {
    let std_dir = tempfile::tempdir().unwrap();
    let (_t, rec, ctx) = lib_ctx(std_dir.path());
    let p = UvProvider::new();
    let a = p
        .plan(&[op(
            OpKind::Install,
            "libtmux",
            Some("kind=lib;python=system"),
            None,
        )])
        .unwrap()
        .remove(0);
    assert!(p.exec(&ctx, &a).unwrap().success);
    assert!(
        rec.lines()
            .iter()
            .any(|l| l.contains("pip install --system libtmux"))
    );
}

/// The opt-in passes `--break-system-packages`, skips the marker check, and asks for root only
/// when the target is not writable.
#[test]
fn break_system_packages_opt_in() {
    let std_dir = tempfile::tempdir().unwrap();
    std::fs::write(std_dir.path().join("EXTERNALLY-MANAGED"), "").unwrap();
    let (_t, rec, ctx) = lib_ctx(std_dir.path());
    let opts = "kind=lib;python=system;break_system_packages=true";
    for (root, want) in [(false, false), (true, true)] {
        let p = UvProvider::new().with_system_root(root);
        let a = p
            .plan(&[op(OpKind::Install, "libtmux", Some(opts), Some("0.37.0"))])
            .unwrap()
            .remove(0);
        assert_eq!(a.needs_root, want);
        assert_eq!(
            a.argv,
            [
                "uv",
                "pip",
                "install",
                "--system",
                "--break-system-packages",
                "libtmux==0.37.0"
            ]
        );
        assert!(p.exec(&ctx, &a).unwrap().success);
    }
    let rm = UvProvider::new()
        .plan(&[op(OpKind::Remove, "libtmux", Some(opts), None)])
        .unwrap()
        .remove(0);
    assert!(
        rm.argv.contains(&"--break-system-packages".to_string()),
        "{:?}",
        rm.argv
    );
    assert!(
        rec.lines()
            .iter()
            .any(|l| l.contains("--break-system-packages"))
    );
}

/// A venv's interpreter is `Scripts\\python.exe` on Windows, and removal does not need `rm`.
#[cfg(windows)]
#[test]
fn venv_kind_uses_scripts_python_and_powershell_on_windows() {
    let p = UvProvider::with_bin("C:/u/uv").with_venv_root("C:/h/venvs");
    let spec = "kind=venv;requirements=requirements.txt;pkg_dir=C:/repo/packages/scripts";
    let actions = p
        .plan(&[op(OpKind::Install, "scripts", Some(spec), None)])
        .unwrap();
    let python = &actions[1].argv[actions[1]
        .argv
        .iter()
        .position(|a| a == "--python")
        .unwrap()
        + 1];
    assert!(python.ends_with("Scripts\\python.exe"), "{python}");
    let rm = argv(&p, op(OpKind::Remove, "scripts", Some("kind=venv"), None));
    assert!(!rm.iter().any(|a| a.contains("/bin/rm")), "{rm:?}");
    assert!(rm.last().unwrap().contains("Remove-Item"), "{rm:?}");
}

/// The git a path source runs, in the provider's lookup order (a brew git comes before the
/// system one), so the faked `git` matches it on any Mac.
fn host_git() -> String {
    ["/opt/homebrew/bin/git", "/usr/local/bin/git", "/usr/bin/git"]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap_or("/usr/bin/git")
        .to_string()
}
