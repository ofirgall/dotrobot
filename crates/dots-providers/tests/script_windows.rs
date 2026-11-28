//! The `script` provider on Windows: scripts run in PowerShell, with real processes in temp
//! dirs. The unix counterpart (bash scripts) is `tests/script.rs`.
#![cfg(windows)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dots_facts::Facts;
use dots_providers::script::{ScriptConfig, ScriptEnv, ScriptProvider};
use dots_providers::{Availability, Ctx, Op, OpKind, Provider, ProviderError, Spec};
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    state: PathBuf,
    ctx: Ctx,
    env: ScriptEnv,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let pkg_dir = root.join("repo").join("packages").join("kv");
    let state = root.join("state");
    let log_dir = state.join("logs").join("run-1");
    for d in [&pkg_dir, &state, &log_dir] {
        fs::create_dir_all(d).unwrap();
    }
    // PowerShell needs the real process environment (SystemRoot and friends).
    let ctx = Ctx {
        log_dir,
        home: root.clone(),
        env: std::env::vars().collect::<BTreeMap<_, _>>(),
        ..Ctx::default()
    };
    let env = ScriptEnv {
        home: root.clone(),
        pkg_dir,
        root: root.join("repo"),
        state: state.clone(),
        bin_dir: root.join("bin"),
        lib_dir: root.join("lib"),
        tags: Vec::new(),
        settings: BTreeMap::new(),
        path_prefix: Vec::new(),
    };
    Sandbox {
        _tmp: tmp,
        state,
        ctx,
        env,
    }
}

fn facts() -> Facts {
    Facts {
        os: "windows".into(),
        arch: "x86_64".into(),
        ..Facts::default()
    }
}

fn provider(sb: &Sandbox, c: ScriptConfig) -> ScriptProvider {
    ScriptProvider::new("kv", c, sb.env.clone(), &facts()).unwrap()
}

/// check: passes once `$DOTS_STATE\installed` exists. (A PowerShell script's exit status is
/// that of its last command, so a failed test needs an explicit `exit 1`.)
const CHECK: &str = "if (-not (Test-Path \"$env:DOTS_STATE/installed\")) { exit 1 }";

fn cfg() -> ScriptConfig {
    ScriptConfig {
        check: Some(CHECK.into()),
        version: Some("'kv 9.9.9' -split ' ' | Select-Object -Last 1".into()),
        install: Some(
            "New-Item -ItemType File -Force \"$env:DOTS_STATE/installed\" | Out-Null\n\
             Set-Content \"$env:DOTS_STATE/pkg.txt\" \"$env:DOTS_PKG\""
                .into(),
        ),
        ..ScriptConfig::default()
    }
}

fn install_op() -> Op {
    Op {
        kind: OpKind::Install,
        spec: Spec::new("kv"),
        version: None,
    }
}

#[test]
fn powershell_is_found_without_path() {
    let sb = sandbox();
    let p = provider(&sb, cfg());
    let mut ctx = sb.ctx.clone();
    ctx.env.clear();
    assert!(
        matches!(
            p.availability_in(&ctx, &facts()),
            Availability::Ready { .. }
        ),
        "no PowerShell found in its known locations"
    );
}

#[test]
fn check_and_version_round_trip_with_install() {
    let sb = sandbox();
    let p = provider(&sb, cfg());
    // Not installed yet: check exits 1.
    assert!(p.installed(&sb.ctx).unwrap().is_empty());
    // Install runs in the package's environment.
    let a = p.plan(&[install_op()]).unwrap();
    let out = p.exec(&sb.ctx, &a[0]).unwrap();
    assert!(out.success);
    assert_eq!(
        fs::read_to_string(sb.state.join("pkg.txt")).unwrap().trim(),
        "kv"
    );
    // Now check passes and the version command's last word is reported.
    let m = p.installed(&sb.ctx).unwrap();
    assert_eq!(m["kv"].version, "9.9.9");
}

#[test]
fn a_failing_install_is_reported_as_a_failure() {
    let sb = sandbox();
    let mut c = cfg();
    c.install = Some("Write-Error 'boom'".into());
    let p = provider(&sb, c);
    let a = p.plan(&[install_op()]).unwrap();
    let out = p.exec(&sb.ctx, &a[0]).unwrap();
    assert!(!out.success);
    assert!(!sb.state.join("installed").exists());
}

#[test]
fn a_hung_script_is_killed_with_its_children_and_reported_as_a_timeout() {
    let sb = sandbox();
    let mut c = cfg();
    // A child process that outlives the script by a minute.
    c.install = Some(
        "Start-Process -NoNewWindow ping -ArgumentList '-n','60','127.0.0.1'\nStart-Sleep 60"
            .into(),
    );
    c.timeout = Some("1s".into());
    let p = provider(&sb, c);
    let start = Instant::now();
    let a = p.plan(&[install_op()]).unwrap();
    let r = p.exec(&sb.ctx, &a[0]);
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "took {:?}",
        start.elapsed()
    );
    match r {
        Err(ProviderError::Coded { code, .. }) => assert_eq!(code, "timeout"),
        other => panic!("expected timeout, got {other:?}"),
    }
}
