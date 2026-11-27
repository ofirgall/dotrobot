//! Tests for the `script` provider with real processes in temp dirs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dots_facts::Facts;
use dots_providers::script::{FetchSpec, ScriptConfig, ScriptEnv, ScriptProvider, parse_duration};
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
        home: PathBuf::new(),
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

/// Records every command; everything succeeds.
#[derive(Debug, Default)]
struct Recorder(std::sync::Mutex<Vec<Vec<String>>>);

impl dots_providers::Runner for Recorder {
    fn run(&self, cmd: &dots_providers::Cmd) -> dots_providers::Result<dots_providers::Output> {
        self.0.lock().unwrap().push(cmd.argv());
        Ok(dots_providers::Output::ok(""))
    }
}

/// The engine wraps a `needs_root` action in `sudo -n`; the post-install check must still run
/// through the shell (unprivileged), not as `sudo -c <text>`.
#[test]
fn check_of_a_root_script_runs_through_the_shell_not_sudo() {
    let sb = sandbox();
    let p = provider(
        &sb,
        ScriptConfig {
            needs_root: true,
            ..cfg("do-install", "is-installed")
        },
    );
    let mut action = p.plan(&[install_op(None)]).unwrap().remove(0);
    let mut argv = vec!["/usr/bin/sudo".to_string(), "-n".to_string()];
    argv.append(&mut action.argv);
    action.argv = argv;
    let rec = std::sync::Arc::new(Recorder::default());
    let ctx = Ctx {
        runner: rec.clone(),
        ..sb.ctx.clone()
    };
    assert!(p.exec(&ctx, &action).unwrap().success);
    let cmds = rec.0.lock().unwrap().clone();
    assert_eq!(cmds.len(), 2, "{cmds:?}");
    assert_eq!(cmds[0][0], "/usr/bin/sudo");
    assert!(
        !cmds[1][0].ends_with("sudo")
            && cmds[1][1] == "-c"
            && cmds[1].last().unwrap().ends_with("is-installed"),
        "check ran as {:?}",
        cmds[1]
    );
}

/// Run a root script's install through a recording runner; `sudo` says whether the engine
/// put `sudo -n` in front. Returns the first command's argv, and the env file's mode and text
/// as read while the command "ran".
fn root_install(sudo: bool) -> (Vec<String>, u32, String, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    type Seen = (Vec<String>, u32, String, PathBuf);
    #[derive(Debug, Default)]
    struct Peek(std::sync::Mutex<Option<Seen>>);
    impl dots_providers::Runner for Peek {
        fn run(&self, cmd: &dots_providers::Cmd) -> dots_providers::Result<dots_providers::Output> {
            let argv = cmd.argv();
            let mut g = self.0.lock().unwrap();
            if g.is_none() {
                let text = argv.last().unwrap();
                let file = text
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("set -a; . "))
                    .and_then(|l| l.strip_suffix("; set +a"))
                    .map(|p| PathBuf::from(p.trim_matches('\'')))
                    .unwrap_or_default();
                let mode = fs::metadata(&file).map_or(0, |m| m.permissions().mode() & 0o777);
                let body = fs::read_to_string(&file).unwrap_or_default();
                *g = Some((argv, mode, body, file));
            }
            Ok(dots_providers::Output::ok(""))
        }
    }
    let mut sb = sandbox();
    sb.env.home = PathBuf::from("/home/ofir");
    sb.env
        .settings
        .insert("token".into(), "s3cr3t'value with space".into());
    let p = provider(
        &sb,
        ScriptConfig {
            needs_root: true,
            ..cfg("do-install", "is-installed")
        },
    );
    let mut action = p.plan(&[install_op(None)]).unwrap().remove(0);
    if sudo {
        let mut argv = vec!["/usr/bin/sudo".to_string(), "-n".to_string()];
        argv.append(&mut action.argv);
        action.argv = argv;
    }
    let peek = std::sync::Arc::new(Peek::default());
    let ctx = Ctx {
        runner: peek.clone(),
        ..sb.ctx.clone()
    };
    assert!(p.exec(&ctx, &action).unwrap().success);
    let got = peek.0.lock().unwrap().clone().unwrap();
    // Gone once the action finished.
    assert!(!got.3.exists(), "env file left behind: {}", got.3.display());
    got
}

/// `sudo -n` resets the environment: HOME, the DOTS_* variables and the `[env]` PATH directories
/// travel in a 0600 file the script text sources, never in argv.
#[test]
fn root_script_carries_env_in_a_private_file_not_argv() {
    let (argv, mode, body, _) = root_install(true);
    assert_eq!(&argv[..2], ["/usr/bin/sudo", "-n"]);
    assert!(
        argv[2].ends_with("sh") || argv[2].ends_with("bash"),
        "{argv:?}"
    );
    assert_eq!(argv[3], "-c");
    assert_eq!(argv.len(), 5, "{argv:?}");
    let text = &argv[4];
    assert!(text.starts_with("set -a; . "), "{text}");
    assert!(text.lines().next().unwrap().ends_with("; set +a"), "{text}");
    assert!(text.ends_with("do-install"), "{text}");
    // No value of any carried variable is in argv.
    for v in ["/home/ofir", "s3cr3t", "work gpu-box", "/usr/bin:/bin"] {
        assert!(argv.iter().all(|a| !a.contains(v)), "{v} in {argv:?}");
    }
    assert_eq!(mode, 0o600);
    assert!(body.contains("HOME=/home/ofir\n"), "{body}");
    assert!(body.contains("DOTS_PKG=kv\n"), "{body}");
    assert!(body.contains("DOTS_TAGS='work gpu-box'\n"), "{body}");
    assert!(
        body.contains("DOTS_SETTING_TOKEN='s3cr3t'\\''value with space'\n"),
        "{body}"
    );
    assert!(body.contains("PATH="), "{body}");
    assert!(
        !body.contains("do-install"),
        "script text stays out of the file"
    );
}

/// Already root: no `sudo`, the same file prelude.
#[test]
fn root_script_without_sudo_gets_the_same_prelude() {
    let (argv, mode, body, _) = root_install(false);
    assert!(
        argv[1] == "-c" && argv[2].starts_with("set -a; . "),
        "{argv:?}"
    );
    assert_eq!(mode, 0o600);
    assert!(body.contains("HOME=/home/ofir\n"));
}

/// The pwsh shell has no such prelude: a root pwsh script is refused with a clear error.
#[test]
fn root_pwsh_script_is_refused() {
    let sb = sandbox();
    let p = provider(
        &sb,
        ScriptConfig {
            needs_root: true,
            shell: Some("pwsh".into()),
            ..cfg("do-install", "is-installed")
        },
    );
    let mut ctx = sb.ctx.clone();
    ctx.runner = std::sync::Arc::new(Recorder::default());
    let action = dots_providers::Action {
        kind: OpKind::Install,
        ops: vec![install_op(None)],
        argv: vec!["/usr/bin/pwsh".into(), "-Command".into(), "x".into()],
        needs_root: true,
    };
    let e = p.exec(&ctx, &action).unwrap_err().to_string();
    assert!(e.contains("needs_root") && e.contains("bash"), "{e}");
}

/// Unprivileged actions are left exactly as planned.
#[test]
fn unprivileged_script_argv_is_unchanged() {
    let sb = sandbox();
    let p = provider(&sb, cfg("do-install", "is-installed"));
    let action = p.plan(&[install_op(None)]).unwrap().remove(0);
    let rec = std::sync::Arc::new(Recorder::default());
    let ctx = Ctx {
        runner: rec.clone(),
        ..sb.ctx.clone()
    };
    assert!(p.exec(&ctx, &action).unwrap().success);
    assert_eq!(rec.0.lock().unwrap()[0], action.argv);
}

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

// ---------------------------------------------------------------------- fetch

mod fetch {
    use super::*;
    use dots_providers::Fetcher;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};

    const INSTALLER: &str = "echo ran > \"$DOTS_STATE/ran\"\n";
    const URL: &str = "https://sh.rustup.rs";

    #[derive(Debug, Default)]
    struct Fake(Mutex<Vec<String>>);
    impl Fetcher for Fake {
        fn fetch(&self, url: &str) -> dots_providers::Result<Vec<u8>> {
            self.0.lock().unwrap().push(url.into());
            Ok(INSTALLER.as_bytes().to_vec())
        }
    }

    fn sha(bytes: &str) -> String {
        Sha256::digest(bytes.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn fetching(sb: &mut Sandbox, pin: Option<String>) -> (ScriptConfig, Arc<Fake>) {
        let fake = Arc::new(Fake::default());
        sb.ctx.fetcher = fake.clone();
        sb.ctx.cache_dir = sb.state.join("cache");
        let c = ScriptConfig {
            install: Some("sh {fetched}".into()),
            check: Some("test -f \"$DOTS_STATE/ran\"".into()),
            versionless: true,
            fetch: Some(FetchSpec {
                url: URL.into(),
                sha256: pin,
            }),
            ..ScriptConfig::default()
        };
        (c, fake)
    }

    #[test]
    fn pinned_installer_runs_with_fetched_path() {
        let mut sb = sandbox();
        let (c, fake) = fetching(&mut sb, Some(format!("sha256:{}", sha(INSTALLER))));
        let p = provider(&sb, c);
        let a = p.plan(&[install_op(None)]).unwrap();
        assert!(
            a[0].argv.last().unwrap().contains("{fetched}"),
            "plan shows the placeholder"
        );
        let out = p.exec(&sb.ctx, &a[0]).unwrap();
        assert!(out.success);
        assert!(sb.state.join("ran").is_file(), "the installer ran");
        assert_eq!(fake.0.lock().unwrap().as_slice(), [URL]);
        assert_eq!(p.fetched_sha256().as_deref(), Some(sha(INSTALLER).as_str()));
        let log = fs::read_to_string(out.log.unwrap()).unwrap();
        assert!(log.contains("dots-fetch-"), "{log}");
        assert!(sb.state.join("cache/fetch/kv/sh.rustup.rs").is_file());
    }

    /// The verified bytes run from a private, exclusively-created copy, never from the
    /// user-writable cache path, and the copy is deleted afterwards.
    #[test]
    fn installer_runs_from_a_private_copy_not_the_cache() {
        let mut sb = sandbox();
        let (c, _) = fetching(&mut sb, None);
        let p = provider(&sb, c);
        let a = p.plan(&[install_op(None)]).unwrap();
        let out = p.exec(&sb.ctx, &a[0]).unwrap();
        assert!(out.success);
        let log = fs::read_to_string(out.log.unwrap()).unwrap();
        assert!(
            !log.contains(&sb.ctx.cache_dir.to_string_lossy().into_owned()),
            "must not run the cache path: {log}"
        );
        let used = log
            .split_whitespace()
            .find(|w| w.contains("dots-fetch-"))
            .expect("private path in log")
            .trim_matches('\'');
        let dir = Path::new(used).parent().unwrap();
        assert!(!dir.exists(), "private copy removed after the run");
    }

    /// A symlink planted at the cache path must not be written through.
    #[cfg(unix)]
    #[test]
    fn cache_write_does_not_follow_a_planted_symlink() {
        let mut sb = sandbox();
        let (c, _) = fetching(&mut sb, None);
        let victim = sb.state.join("victim");
        fs::write(&victim, "precious").unwrap();
        let cached = sb.state.join("cache/fetch/kv");
        fs::create_dir_all(&cached).unwrap();
        std::os::unix::fs::symlink(&victim, cached.join("sh.rustup.rs")).unwrap();
        let p = provider(&sb, c);
        assert!(run_install(&p, &sb, None).unwrap().success);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
    }

    #[test]
    fn hash_mismatch_refuses_and_runs_nothing() {
        let mut sb = sandbox();
        let (c, _) = fetching(&mut sb, Some("0".repeat(64)));
        let p = provider(&sb, c);
        match run_install(&p, &sb, None) {
            Err(ProviderError::Coded { code, message, .. }) => {
                assert_eq!(code, "integrity-mismatch");
                assert!(
                    message.contains(&sha(INSTALLER)) && message.contains(URL),
                    "{message}"
                );
            }
            other => panic!("expected integrity-mismatch, got {other:?}"),
        }
        assert!(!sb.state.join("ran").exists());
        assert_eq!(p.fetched_sha256(), None);
    }

    #[test]
    fn lock_pin_must_match_too() {
        let mut sb = sandbox();
        let (c, _) = fetching(&mut sb, None);
        let p = provider(&sb, c.clone()).with_lock_pin(Some("f".repeat(64)));
        assert!(matches!(
            run_install(&p, &sb, None),
            Err(ProviderError::Coded {
                code: "integrity-mismatch",
                ..
            })
        ));
        assert!(!sb.state.join("ran").exists());
        let p = provider(&sb, c).with_lock_pin(Some(sha(INSTALLER)));
        assert!(run_install(&p, &sb, None).unwrap().success);
    }

    #[test]
    fn first_use_reports_the_hash_to_record() {
        let mut sb = sandbox();
        let (c, _) = fetching(&mut sb, None);
        let p = provider(&sb, c);
        assert_eq!(p.fetched_sha256(), None);
        assert!(run_install(&p, &sb, None).unwrap().success);
        assert_eq!(p.fetched_sha256().as_deref(), Some(sha(INSTALLER).as_str()));
    }

    #[test]
    fn offline_and_dry_run_download_nothing() {
        let mut sb = sandbox();
        let (c, fake) = fetching(&mut sb, None);
        let p = provider(&sb, c);
        sb.ctx.offline = true;
        assert!(run_install(&p, &sb, None).is_err());
        sb.ctx.offline = false;
        sb.ctx.dry_run = true;
        assert!(run_install(&p, &sb, None).unwrap().success);
        assert!(fake.0.lock().unwrap().is_empty());
    }

    #[test]
    fn fetch_needs_a_placeholder_and_a_web_url() {
        let sb = sandbox();
        let mut c = cfg("sh installer.sh", "true");
        c.fetch = Some(FetchSpec {
            url: URL.into(),
            sha256: None,
        });
        assert!(ScriptProvider::new("kv", c.clone(), sb.env.clone(), &facts()).is_err());
        c.install = Some("sh {fetched}".into());
        assert!(ScriptProvider::new("kv", c.clone(), sb.env.clone(), &facts()).is_ok());
        c.fetch = Some(FetchSpec {
            url: "file:///etc/passwd".into(),
            sha256: None,
        });
        assert!(ScriptProvider::new("kv", c, sb.env.clone(), &facts()).is_err());
    }
}

// ---------------------------------------------------------------- file or shell text

/// `install = "install.sh"`: one token that names a file in the package directory runs that
/// file with the script's shell (cwd = the package directory, no executable bit needed);
/// anything else is shell text. Same for `upgrade`, `uninstall`, `check`, `version`,
/// `latest` and hooks.
#[test]
fn a_single_token_naming_a_file_in_the_package_dir_runs_as_a_file() {
    let sb = sandbox();
    fs::write(
        sb.pkg_dir.join("install.sh"),
        "pwd > \"$DOTS_STATE/where\"\necho \"args=$#\" >> \"$DOTS_STATE/where\"\ntouch \"$DOTS_STATE/ok\"\n",
    )
    .unwrap();
    fs::write(sb.pkg_dir.join("check.sh"), "test -f \"$DOTS_STATE/ok\"\n").unwrap();
    fs::write(sb.pkg_dir.join("version.sh"), "echo 4.5.6\n").unwrap();
    fs::write(sb.pkg_dir.join("bye.sh"), "rm -f \"$DOTS_STATE/ok\"\n").unwrap();
    let mut c = cfg("install.sh", "check.sh");
    c.version = Some("version.sh".into());
    c.uninstall = Some("bye.sh".into());
    let p = provider(&sb, c);
    assert!(
        p.installed(&sb.ctx).unwrap().is_empty(),
        "not installed yet"
    );
    let plan = p.plan(&[install_op(None)]).unwrap();
    assert!(
        plan[0].argv.last().unwrap().contains("bash ./install.sh"),
        "{:?}",
        plan[0].argv
    );
    assert!(run_install(&p, &sb, None).unwrap().success);
    let where_ = fs::read_to_string(sb.state.join("where")).unwrap();
    let mut lines = where_.lines();
    assert_eq!(
        fs::canonicalize(lines.next().unwrap()).unwrap(),
        fs::canonicalize(&sb.pkg_dir).unwrap()
    );
    assert_eq!(lines.next(), Some("args=0"));
    assert_eq!(p.installed(&sb.ctx).unwrap()["kv"].version, "4.5.6");
    // uninstall and a hook run their files the same way.
    let rm = Op {
        kind: OpKind::Remove,
        ..install_op(None)
    };
    let a = p.plan(&[rm]).unwrap();
    assert!(p.exec(&sb.ctx, &a[0]).unwrap().success);
    assert!(!sb.state.join("ok").exists());
    fs::write(sb.pkg_dir.join("hook.sh"), "touch \"$DOTS_STATE/hooked\"\n").unwrap();
    let cmd = p.hook_cmd(&sb.ctx, "hook.sh").unwrap();
    assert!(
        <dots_providers::RealRunner as dots_providers::Runner>::run(
            &dots_providers::RealRunner,
            &cmd
        )
        .map(|o| o.success())
        .unwrap()
    );
    assert!(sb.state.join("hooked").exists());
}

#[test]
fn anything_that_is_not_one_existing_file_stays_shell_text() {
    let sb = sandbox();
    fs::write(
        sb.pkg_dir.join("install.sh"),
        "touch \"$DOTS_STATE/from-file\"\n",
    )
    .unwrap();
    // A file named as the first word of a longer command is not run as a file.
    let p = provider(&sb, cfg("true install.sh", "true"));
    let a = p.plan(&[install_op(None)]).unwrap();
    assert!(a[0].argv.last().unwrap().ends_with("true install.sh"));
    // A single token with no such file is the command itself.
    let p = provider(&sb, cfg("touch-me", "true"));
    let a = p.plan(&[install_op(None)]).unwrap();
    assert!(a[0].argv.last().unwrap().ends_with("touch-me"));
    assert!(!a[0].argv.last().unwrap().contains("bash"));
    // A plain command word that happens to be a file name in another form is quoted shell
    // text; here `true` has no file next to the package and runs as the builtin.
    let p = provider(&sb, cfg("true", "true"));
    assert!(run_install(&p, &sb, None).unwrap().success);
    assert!(!sb.state.join("from-file").exists());
}

#[test]
fn the_lock_key_is_exposed_to_the_engine() {
    let sb = sandbox();
    let mut c = cfg("true", "true");
    assert_eq!(provider(&sb, c.clone()).lock_key(), None);
    c.lock = Some("pi".into());
    assert_eq!(provider(&sb, c).lock_key().as_deref(), Some("pi"));
}

fn short_queries(mut sb: Sandbox) -> Sandbox {
    sb.ctx.query_timeout = Duration::from_millis(300);
    sb
}

#[test]
fn a_hung_check_is_an_error_for_that_package_within_the_query_timeout() {
    let sb = short_queries(sandbox());
    let p = provider(&sb, cfg("true", "sleep 30"));
    let start = Instant::now();
    let r = p.installed(&sb.ctx);
    assert!(start.elapsed() < Duration::from_secs(10));
    match r {
        Err(ProviderError::Coded { code, message, .. }) => {
            assert_eq!(code, "query-timeout");
            assert!(
                message.contains("`kv`")
                    && message.contains("300ms")
                    && message.contains("sleep 30"),
                "{message}"
            );
        }
        other => panic!("expected query-timeout, got {other:?}"),
    }
}

#[test]
fn a_hung_version_is_unknown_with_a_warning_not_an_error() {
    let sb = short_queries(sandbox());
    let mut c = cfg("true", "true");
    c.version = Some("sleep 30".into());
    let p = provider(&sb, c);
    let start = Instant::now();
    let m = p.installed(&sb.ctx).unwrap();
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(m["kv"].version, "unknown");
    let t = dots_providers::take_query_timeouts();
    assert!(
        t.iter().any(|t| t.package == "kv" && t.what == "version"),
        "{t:?}"
    );
}

#[test]
fn a_hung_latest_is_no_answer_with_a_warning() {
    let sb = short_queries(sandbox());
    let mut c = cfg("true", "true");
    c.latest = Some("sleep 30".into());
    let p = provider(&sb, c);
    let m = p.latest(&sb.ctx, &[Spec::new("kv")]).unwrap();
    assert!(m.is_empty());
    let t = dots_providers::take_query_timeouts();
    assert!(t.iter().any(|t| t.what == "latest"), "{t:?}");
}
