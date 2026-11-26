//! Tests for the `script` provider with real processes in temp dirs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dots_facts::Facts;
use dots_providers::script::{ScriptConfig, ScriptEnv, ScriptProvider, parse_duration};
use dots_providers::{Availability, Ctx, Op, OpKind, Provider, ProviderError, Spec};
use tempfile::TempDir;

fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/script")
        .join(name);
    fs::read_to_string(p).unwrap()
}

struct Sandbox {
    _tmp: TempDir,
    pkg_dir: PathBuf,
    state: PathBuf,
    ctx: Ctx,
    env: ScriptEnv,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let pkg_dir = root.join("repo/packages/kv");
    let state = root.join("state");
    let log_dir = state.join("logs/run-1");
    for d in [
        &pkg_dir,
        &state,
        &log_dir,
        &root.join("bin"),
        &root.join("lib"),
    ] {
        fs::create_dir_all(d).unwrap();
    }
    let ctx = Ctx {
        log_dir,
        home: root.clone(),
        env: BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        ..Ctx::default()
    };
    let env = ScriptEnv {
        pkg_dir: pkg_dir.clone(),
        root: root.join("repo"),
        state: state.clone(),
        bin_dir: root.join("bin"),
        lib_dir: root.join("lib"),
        tags: vec!["work".into(), "gpu-box".into()],
        settings: BTreeMap::from([("font_size".to_string(), "15".to_string())]),
        path_prefix: vec![root.join("bin")],
    };
    Sandbox {
        _tmp: tmp,
        pkg_dir,
        state,
        ctx,
        env,
    }
}

fn facts() -> Facts {
    Facts {
        os: "linux".into(),
        arch: "x86_64".into(),
        distro: Some("ubuntu".into()),
        has_sudo: true,
        ..Facts::default()
    }
}

fn cfg(install: &str, check: &str) -> ScriptConfig {
    ScriptConfig {
        install: Some(install.into()),
        check: Some(check.into()),
        version: Some("echo 1.2.3".into()),
        ..ScriptConfig::default()
    }
}

fn provider(sb: &Sandbox, c: ScriptConfig) -> ScriptProvider {
    ScriptProvider::new("kv", c, sb.env.clone(), &facts()).unwrap()
}

fn install_op(version: Option<&str>) -> Op {
    Op {
        kind: OpKind::Install,
        spec: Spec::new("kv"),
        version: version.map(String::from),
    }
}

fn run_install(
    p: &ScriptProvider,
    sb: &Sandbox,
    version: Option<&str>,
) -> dots_providers::Result<dots_providers::Outcome> {
    let a = p.plan(&[install_op(version)]).unwrap();
    p.exec(&sb.ctx, &a[0])
}

#[test]
fn runtime_environment_cwd_stdin_and_variables() {
    let sb = sandbox();
    let c = cfg(&fixture("dump-env.sh"), "test -f \"$DOTS_STATE/installed\"");
    let p = provider(&sb, c);
    let out = run_install(&p, &sb, Some("2.0.0")).unwrap();
    assert!(out.success);

    let cwd = fs::read_to_string(sb.state.join("cwd.txt")).unwrap();
    assert_eq!(
        fs::canonicalize(cwd.trim()).unwrap(),
        fs::canonicalize(&sb.pkg_dir).unwrap()
    );
    assert_eq!(
        fs::read_to_string(sb.state.join("stdin.txt"))
            .unwrap()
            .trim(),
        "stdin-eof"
    );
    let env = fs::read_to_string(sb.state.join("env.txt")).unwrap();
    let get = |k: &str| {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("{k} missing from:\n{env}"))
            .to_string()
    };
    assert_eq!(get("DOTS_OS"), "linux");
    assert_eq!(get("DOTS_ARCH"), "x86_64");
    assert_eq!(get("DOTS_DISTRO"), "ubuntu");
    assert_eq!(get("DOTS_WSL"), "0");
    assert_eq!(get("DOTS_SUDO"), "1");
    assert_eq!(get("DOTS_TAGS"), "work gpu-box");
    assert_eq!(get("DOTS_TAG_WORK"), "1");
    assert_eq!(get("DOTS_TAG_GPU_BOX"), "1");
    assert_eq!(get("DOTS_PKG"), "kv");
    assert_eq!(get("DOTS_PKG_DIR"), sb.pkg_dir.to_string_lossy());
    assert_eq!(get("DOTS_ROOT"), sb.env.root.to_string_lossy());
    assert_eq!(get("DOTS_STATE"), sb.state.to_string_lossy());
    assert_eq!(get("DOTS_BIN"), sb.env.bin_dir.to_string_lossy());
    assert_eq!(get("DOTS_LIB"), sb.env.lib_dir.to_string_lossy());
    assert_eq!(get("DOTS_VERSION_WANTED"), "2.0.0");
    assert_eq!(get("DOTS_DRY_RUN"), "0");
    assert_eq!(get("DOTS_SETTING_FONT_SIZE"), "15");
}

#[test]
fn path_is_extended_with_provider_bins() {
    let sb = sandbox();
    let c = cfg(
        "echo \"$PATH\" > \"$DOTS_STATE/path.txt\"; touch \"$DOTS_STATE/ok\"",
        "test -f \"$DOTS_STATE/ok\"",
    );
    let p = provider(&sb, c);
    run_install(&p, &sb, None).unwrap();
    let path = fs::read_to_string(sb.state.join("path.txt")).unwrap();
    let bin = sb.env.bin_dir.to_string_lossy().into_owned();
    assert!(path.starts_with(&format!("{bin}:")), "{path}");
    assert!(path.trim_end().ends_with("/usr/bin:/bin"), "{path}");
}

#[test]
fn output_goes_to_the_per_action_log() {
    let sb = sandbox();
    let p = provider(
        &sb,
        cfg(
            "echo from-stdout; echo from-stderr >&2; touch \"$DOTS_STATE/ok\"",
            "test -f \"$DOTS_STATE/ok\"",
        ),
    );
    let out = run_install(&p, &sb, None).unwrap();
    let log = out.log.unwrap();
    assert_eq!(log, sb.ctx.log_dir.join("kv.install.log"));
    let text = fs::read_to_string(log).unwrap();
    assert!(text.contains("from-stdout") && text.contains("from-stderr"));
}

#[test]
fn install_that_lies_fails_with_check_failed_after_install() {
    let sb = sandbox();
    let p = provider(&sb, cfg("echo pretending", "exit 1"));
    match run_install(&p, &sb, None) {
        Err(ProviderError::Coded { code, message, log }) => {
            assert_eq!(code, "check-failed-after-install");
            assert!(
                message.contains("succeeded but its check fails"),
                "{message}"
            );
            let log = log.expect("log linked");
            assert!(message.contains(&log.display().to_string()));
            let text = fs::read_to_string(log).unwrap();
            assert!(text.contains("pretending"));
        }
        other => panic!("expected check-failed-after-install, got {other:?}"),
    }
}

#[test]
fn failing_install_reports_failure_and_skips_check() {
    let sb = sandbox();
    let p = provider(
        &sb,
        cfg("echo boom >&2; exit 3", "touch \"$DOTS_STATE/check-ran\""),
    );
    let out = run_install(&p, &sb, None).unwrap();
    assert!(!out.success);
    assert!(!sb.state.join("check-ran").exists());
}

#[test]
fn bash_scripts_stop_at_the_first_error() {
    let sb = sandbox();
    let p = provider(&sb, cfg("false; touch \"$DOTS_STATE/after\"", "true"));
    let out = run_install(&p, &sb, None).unwrap();
    assert!(!out.success);
    assert!(!sb.state.join("after").exists());
}

#[test]
fn hung_script_is_killed_with_its_children_and_reported_as_timeout() {
    let sb = sandbox();
    let mut c = cfg(&fixture("hang.sh"), "true");
    c.timeout = Some("400ms".into());
    let p = provider(&sb, c);
    let start = Instant::now();
    let r = run_install(&p, &sb, None);
    // The background `sleep 60` must not keep the output pipes (and us) waiting.
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "took {:?}",
        start.elapsed()
    );
    match r {
        Err(ProviderError::Coded { code, message, .. }) => {
            assert_eq!(code, "timeout");
            assert!(
                message.contains("exceeded its timeout of 400ms"),
                "{message}"
            );
        }
        other => panic!("expected timeout, got {other:?}"),
    }
}

#[test]
fn installed_reports_version_when_check_passes() {
    let sb = sandbox();
    let mut c = cfg("true", "true");
    c.version = Some("echo 'kv 9.9.9' | awk '{print $2}'".into());
    let p = provider(&sb, c);
    let m = p.installed(&sb.ctx).unwrap();
    assert_eq!(m["kv"].version, "9.9.9");
}

#[test]
fn installed_is_empty_when_check_fails() {
    let sb = sandbox();
    let p = provider(&sb, cfg("true", "exit 1"));
    assert!(p.installed(&sb.ctx).unwrap().is_empty());
}

#[test]
fn versionless_is_installed_but_unverified() {
    let sb = sandbox();
    let c = ScriptConfig {
        install: Some("true".into()),
        check: Some("true".into()),
        versionless: true,
        ..ScriptConfig::default()
    };
    let p = provider(&sb, c);
    assert_eq!(p.installed(&sb.ctx).unwrap()["kv"].version, "unknown");
}

#[test]
fn latest_runs_the_latest_command() {
    let sb = sandbox();
    let mut c = cfg("true", "true");
    c.latest = Some("echo abc123".into());
    let p = provider(&sb, c);
    let m = p.latest(&sb.ctx, &[Spec::new("kv")]).unwrap();
    assert_eq!(m["kv"].version, "abc123");
    let none = provider(&sb, cfg("true", "true"));
    assert!(none.latest(&sb.ctx, &[Spec::new("kv")]).unwrap().is_empty());
}

#[test]
fn plan_is_pure_and_shows_the_exact_command() {
    let sb = sandbox();
    let p = provider(&sb, cfg("scripts/install.sh", "true"));
    let a = p.plan(&[install_op(None)]).unwrap();
    assert_eq!(a.len(), 1);
    assert!(a[0].argv.last().unwrap().ends_with("scripts/install.sh"));
    assert_eq!(a, p.plan(&[install_op(None)]).unwrap());
}

#[test]
fn upgrade_defaults_to_install_and_uninstall_is_optional() {
    let sb = sandbox();
    let mut c = cfg("echo inst", "true");
    c.upgrade = None;
    let p = provider(&sb, c);
    let up = p
        .plan(&[Op {
            kind: OpKind::Upgrade,
            ..install_op(None)
        }])
        .unwrap();
    assert!(up[0].argv.last().unwrap().contains("echo inst"));
    assert!(!p.caps().uninstall);
    let rm = Op {
        kind: OpKind::Remove,
        ..install_op(None)
    };
    let e = p.plan(std::slice::from_ref(&rm)).unwrap_err().to_string();
    assert!(e.contains("cannot uninstall"), "{e}");

    let mut c = cfg("true", "true");
    c.uninstall = Some("echo bye".into());
    let p = provider(&sb, c);
    assert!(p.caps().uninstall);
    let a = p.plan(&[rm]).unwrap();
    let out = p.exec(&sb.ctx, &a[0]).unwrap();
    assert!(out.success);
    assert_eq!(out.log.unwrap(), sb.ctx.log_dir.join("kv.uninstall.log"));
}

#[test]
fn required_keys_are_enforced() {
    let f = facts();
    let env = ScriptEnv::default();
    let mk = |c: ScriptConfig| {
        ScriptProvider::new("kv", c, env.clone(), &f)
            .err()
            .map(|e| e.to_string())
    };
    assert!(mk(ScriptConfig::default()).unwrap().contains("check"));
    let c = ScriptConfig {
        check: Some("true".into()),
        ..ScriptConfig::default()
    };
    assert!(mk(c.clone()).unwrap().contains("install"));
    let c = ScriptConfig {
        install: Some("true".into()),
        ..c
    };
    assert!(mk(c.clone()).unwrap().contains("version"));
    assert!(
        mk(ScriptConfig {
            versionless: true,
            ..c.clone()
        })
        .is_none()
    );
    assert!(
        mk(ScriptConfig {
            timeout: Some("soon".into()),
            versionless: true,
            ..c
        })
        .unwrap()
        .contains("timeout")
    );
}

#[test]
fn caps_follow_the_config() {
    let sb = sandbox();
    let mut c = cfg("true", "true");
    c.needs_root = true;
    c.interactive = true;
    let caps = provider(&sb, c).caps();
    assert!(caps.needs_root && caps.exclusive_lock && !caps.batch_install && !caps.pin);
    let a = provider(
        &sb,
        ScriptConfig {
            needs_root: true,
            ..cfg("true", "true")
        },
    )
    .plan(&[install_op(None)])
    .unwrap();
    assert!(a[0].needs_root);
}

#[test]
fn availability_finds_a_shell_without_path() {
    let sb = sandbox();
    let p = provider(&sb, cfg("true", "true"));
    match p.availability_in(&sb.ctx, &facts()) {
        Availability::Ready { bin_path } => assert!(bin_path.is_absolute()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn durations_parse() {
    assert_eq!(parse_duration("20m").unwrap(), Duration::from_secs(1200));
    assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
    assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
    assert!(
        parse_duration("").is_err()
            && parse_duration("m").is_err()
            && parse_duration("5x").is_err()
    );
}

/// 3.8: every variable the spec lists for scripts appears in `docs/providers.md`.
#[test]
fn docs_mention_every_script_variable_of_the_spec() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let doc = fs::read_to_string(root.join("docs/providers.md")).unwrap();
    let spec_path = root.join("openspec/changes/implement-dots-v1/specs/provider-contract/spec.md");
    let mut vars: Vec<String> = [
        "DOTS_OS",
        "DOTS_ARCH",
        "DOTS_DISTRO",
        "DOTS_WSL",
        "DOTS_SUDO",
        "DOTS_TAGS",
        "DOTS_TAG_<NAME>",
        "DOTS_PKG",
        "DOTS_PKG_DIR",
        "DOTS_ROOT",
        "DOTS_STATE",
        "DOTS_BIN",
        "DOTS_LIB",
        "DOTS_VERSION_WANTED",
        "DOTS_DRY_RUN",
        "DOTS_SETTING_<NAME>",
    ]
    .map(String::from)
    .to_vec();
    // The change may be archived later; then the hardcoded list above is the contract.
    if let Ok(spec) = fs::read_to_string(&spec_path) {
        let line = spec
            .lines()
            .find(|l| l.contains("it sees `DOTS_OS`"))
            .expect("spec lists the script variables");
        let from_spec: Vec<String> = line
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|t| t.starts_with("DOTS_"))
            .map(String::from)
            .collect();
        assert_eq!(from_spec, vars, "spec list and the hardcoded list differ");
        vars = from_spec;
    }
    for v in vars {
        assert!(
            doc.contains(&format!("`{v}`")),
            "docs/providers.md does not mention `{v}`"
        );
    }
}

// ---- conformance ---------------------------------------------------------------------------

/// An in-memory shell: the install script flips a flag, the check and the combined probe read it.
#[derive(Debug, Default)]
struct FakeShell {
    installed: std::sync::atomic::AtomicBool,
}

impl dots_providers::Runner for FakeShell {
    fn run(&self, cmd: &dots_providers::Cmd) -> dots_providers::Result<dots_providers::Output> {
        use std::sync::atomic::Ordering::SeqCst;
        let text = cmd.args.last().cloned().unwrap_or_default();
        let have = self.installed.load(SeqCst);
        let out = if text.contains("__dots_installed__") {
            dots_providers::Output::ok(if have {
                "__dots_installed__\n1.2.3\n"
            } else {
                ""
            })
        } else if text.ends_with("do-install") {
            self.installed.store(true, SeqCst);
            dots_providers::Output::ok("")
        } else if text.ends_with("is-installed") && have {
            dots_providers::Output::ok("")
        } else {
            dots_providers::Output::fail(1, "no")
        };
        Ok(out)
    }
}

dots_testkit::conformance_test!(script_conforms, {
    let sb = sandbox();
    let c = ScriptConfig {
        uninstall: Some("do-remove".into()),
        ..cfg("do-install", "is-installed")
    };
    let p = provider(&sb, c);
    let fx = dots_testkit::ConformanceFixture::new("kv", "kv")
        .single_package()
        .backend(std::sync::Arc::new(FakeShell::default()));
    // The sandbox must outlive the test body; leaking one temp dir is fine here.
    std::mem::forget(sb);
    (p, fx)
});

#[test]
fn exec_runs_nothing_under_dry_run() {
    let sb = sandbox();
    let p = provider(&sb, cfg("touch ran", "false"));
    let a = p.plan(&[install_op(None)]).unwrap();
    let ctx = Ctx {
        dry_run: true,
        ..sb.ctx.clone()
    };
    assert!(p.exec(&ctx, &a[0]).unwrap().success);
    assert!(!sb.pkg_dir.join("ran").exists());
}
