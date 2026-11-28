//! Tests for the brew provider: conformance against a stateful fake brew, scripted
//! `brew info --json=v2` fixtures captured from a real Homebrew, and a read-only run against the
//! real brew when this machine has one.

#[cfg_attr(not(unix), allow(unused_imports))] // used by unix-only tests
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dots_facts::Facts;
use dots_providers::brew::{self, BrewProvider};
#[cfg_attr(not(unix), allow(unused_imports))] // used by unix-only tests
use dots_providers::{
    Availability, Cmd, Ctx, Installed, Op, OpKind, Output, Provider, RealRunner, Result, Runner,
    Spec,
};
use dots_testkit::{ConformanceFixture, RecordingRunner, conformance_test};

const INSTALLED: &str = include_str!("fixtures/brew/installed.json");
const LATEST_FORMULAE: &str = include_str!("fixtures/brew/latest_formulae.json");
const LATEST_CASKS: &str = include_str!("fixtures/brew/latest_casks.json");

/// Create `<root>/bin/brew` (an empty file: commands go through the scripted runner).
fn fake_prefix(root: &Path) -> PathBuf {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    let brew = root.join("bin/brew");
    std::fs::write(&brew, "#!/bin/sh\n").unwrap();
    brew
}

fn facts(os: &str) -> Facts {
    Facts {
        os: os.into(),
        ..Facts::default()
    }
}

fn ctx_with(runner: Arc<dyn Runner>, tmp: &Path) -> Ctx {
    Ctx {
        runner,
        home: tmp.join("home"),
        log_dir: tmp.join("logs/run-1"),
        ..Ctx::default()
    }
}

fn op(kind: OpKind, spec: Spec, version: Option<&str>) -> Op {
    Op {
        kind,
        spec,
        version: version.map(str::to_string),
    }
}

fn argv(a: &dots_providers::Action) -> String {
    a.argv[1..].join(" ")
}

// ---- conformance --------------------------------------------------------------------------

/// A tiny stateful brew: `info --json=v2 --installed` and `install`.
#[derive(Debug, Default)]
struct FakeBrew {
    extra: Mutex<Vec<String>>,
}

impl Runner for FakeBrew {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let a: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["info", "--json=v2", "--installed"] => {
                let mut doc: serde_json::Value = serde_json::from_str(INSTALLED).unwrap();
                for n in self.extra.lock().unwrap().iter() {
                    doc["formulae"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "name": n, "tap": "homebrew/core", "linked_keg": "1.0",
                            "installed": [{"version": "1.0"}], "pinned": false
                        }));
                }
                Ok(Output::ok(&doc.to_string()))
            }
            ["install", names @ ..] => {
                self.extra
                    .lock()
                    .unwrap()
                    .extend(names.iter().map(|s| s.to_string()));
                Ok(Output::ok("==> Installed\n"))
            }
            other => Ok(Output::fail(
                1,
                &format!("fake brew: unsupported {other:?}"),
            )),
        }
    }
}

conformance_test!(brew_conforms_with_fake_backend, {
    let fx = ConformanceFixture::new("bat", "fd").backend(Arc::new(FakeBrew::default()));
    let bin = fake_prefix(&fx.sandbox_dir().join("prefix"));
    (BrewProvider::with_candidates(vec![bin]), fx)
});

// ---- installed / latest -------------------------------------------------------------------

fn scripted(tmp: &Path) -> (BrewProvider, Arc<RecordingRunner>, Ctx, String) {
    let bin = fake_prefix(&tmp.join("prefix"));
    let b = bin.to_string_lossy().into_owned();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = ctx_with(rec.clone(), tmp);
    (BrewProvider::with_candidates(vec![bin]), rec, ctx, b)
}

#[test]
fn installed_is_one_call_and_maps_formulae_and_casks() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, rec, ctx, b) = scripted(tmp.path());
    rec.on(
        &[&b, "info", "--json=v2", "--installed"],
        Output::ok(INSTALLED),
    );
    let m = p.installed(&ctx).unwrap();
    assert_eq!(rec.lines(), [format!("{b} info --json=v2 --installed")]);
    let env = &rec.commands()[0].env;
    assert_eq!(env["HOMEBREW_NO_AUTO_UPDATE"], "1");
    assert_eq!(env["HOMEBREW_NO_INSTALL_CLEANUP"], "1");

    assert_eq!(m["bat"].version, "0.26.1");
    assert!(!m["bat"].pinned);
    assert_eq!(m["bat"].source, None);
    // revisions are part of brew's version string; the linked keg wins over older kegs.
    assert_eq!(m["ffmpeg"].version, "9.0.2_1");
    assert_eq!(m["jpeg-turbo"].version, "3.2.1");
    // keg-only formulae have no linked keg: newest installed one.
    assert_eq!(m["llvm"].version, "22.1.8");
    assert!(m["ripgrep"].pinned);
    assert_eq!(m["acli"].source.as_deref(), Some("atlassian/acli"));
    // casks
    assert_eq!(m["1password-cli"].version, "2.35.0");
    assert_eq!(m["1password-cli"].source.as_deref(), Some("cask"));
    assert_eq!(
        m["aerospace"].source.as_deref(),
        Some("cask:nikitabobko/tap")
    );
    assert_eq!(m["orbstack"].version, "2.2.3,20963");
}

#[test]
fn installed_failure_and_missing_brew_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, rec, ctx, _) = scripted(tmp.path());
    rec.on(&[], Output::fail(1, "Error: boom\n"));
    let e = p.installed(&ctx).unwrap_err().to_string();
    assert!(e.contains("boom"), "{e}");
    let none = BrewProvider::with_candidates(vec![tmp.path().join("nope/bin/brew")]);
    assert!(none.installed(&ctx).is_err());
}

#[test]
fn latest_is_batched_per_kind_and_offline_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, rec, mut ctx, b) = scripted(tmp.path());
    rec.on(
        &[&b, "info", "--json=v2", "--cask"],
        Output::ok(LATEST_CASKS),
    );
    rec.on(&[&b, "info", "--json=v2"], Output::ok(LATEST_FORMULAE));
    let specs = [
        Spec::new("bat"),
        Spec::new("ffmpeg"),
        brew::spec("1password-cli", true, None),
        Spec::new("ripgrep"),
    ];
    let m = p.latest(&ctx, &specs).unwrap();
    assert_eq!(
        rec.lines(),
        [
            format!("{b} info --json=v2 bat ffmpeg ripgrep"),
            format!("{b} info --json=v2 --cask 1password-cli"),
        ]
    );
    assert_eq!(m["bat"].version, "0.26.1");
    assert_eq!(m["ffmpeg"].version, "9.0.2_1");
    assert_eq!(m["1password-cli"].version, "2.42.0");

    rec.clear();
    ctx.offline = true;
    assert!(p.latest(&ctx, &specs).unwrap().is_empty());
    assert_eq!(rec.count(), 0);
}

// ---- plan ---------------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn plan_batches_formulae_casks_taps_and_pins() {
    let p = BrewProvider::new();
    let ops = [
        op(OpKind::Install, Spec::new("a"), None),
        op(OpKind::Install, brew::spec("x", true, None), None),
        op(
            OpKind::Install,
            brew::spec("t", false, Some("me/tools")),
            None,
        ),
        op(OpKind::Install, Spec::new("b"), None),
        op(OpKind::Upgrade, Spec::new("c"), None),
        op(OpKind::Upgrade, brew::spec("y", true, None), None),
        op(OpKind::Remove, Spec::new("d"), None),
        op(OpKind::Pin, Spec::new("a"), None),
        op(OpKind::Unpin, Spec::new("c"), None),
    ];
    let acts = p.plan(&ops).unwrap();
    let lines: Vec<String> = acts.iter().map(argv).collect();
    assert_eq!(
        lines,
        [
            "tap me/tools",
            "unpin c",
            "uninstall d",
            "install a me/tools/t b",
            "install --cask x",
            "upgrade c",
            "upgrade --cask y",
            "pin a",
        ]
    );
    assert!(
        acts.iter()
            .all(|a| Path::new(&a.argv[0]).is_absolute() && !a.needs_root)
    );
    // every op is carried by exactly one action; the tap action carries none.
    assert_eq!(acts.iter().map(|a| a.ops.len()).sum::<usize>(), ops.len());
    assert_eq!(p.plan(&ops).unwrap(), acts, "plan is deterministic");
}

/// `brew install --cask a b c` stops at the first failure: the retry is one command per
/// package, casks keep `--cask`, taps are not repeated, and only installs and upgrades of
/// several packages are retried.
#[test]
fn retry_each_splits_a_batch_into_one_command_per_package() {
    let p = BrewProvider::new();
    let ops = [
        op(OpKind::Install, brew::spec("a", true, None), None),
        op(OpKind::Install, brew::spec("b", true, None), None),
        op(OpKind::Install, brew::spec("c", true, None), None),
        op(OpKind::Install, Spec::new("f1"), None),
        op(
            OpKind::Install,
            brew::spec("f2", false, Some("me/tools")),
            None,
        ),
        op(OpKind::Upgrade, Spec::new("u1"), None),
        op(OpKind::Upgrade, Spec::new("u2"), None),
    ];
    let acts = p.plan(&ops).unwrap();
    let batch = |kind, cask: bool| {
        acts.iter()
            .find(|a| {
                !a.ops.is_empty() && a.kind == kind && a.argv.iter().any(|x| x == "--cask") == cask
            })
            .unwrap()
    };
    let lines = |a: &dots_providers::Action| -> Vec<String> {
        p.retry_each(a).unwrap().iter().map(argv).collect()
    };
    assert_eq!(
        lines(batch(OpKind::Install, true)),
        ["install --cask a", "install --cask b", "install --cask c"]
    );
    assert_eq!(
        lines(batch(OpKind::Install, false)),
        ["install f1", "install me/tools/f2"]
    );
    assert_eq!(
        lines(batch(OpKind::Upgrade, false)),
        ["upgrade u1", "upgrade u2"]
    );
    // Each retry carries exactly its own op, so the engine can attribute the outcome.
    for a in p.retry_each(batch(OpKind::Install, true)).unwrap() {
        assert_eq!(a.ops.len(), 1);
    }
    // One package, a removal, a tap: nothing to split.
    let one = p
        .plan(&[op(OpKind::Install, brew::spec("a", true, None), None)])
        .unwrap();
    assert!(p.retry_each(&one[0]).is_none());
    let rm = p
        .plan(&[
            op(OpKind::Remove, Spec::new("x"), None),
            op(OpKind::Remove, Spec::new("y"), None),
        ])
        .unwrap();
    assert!(p.retry_each(&rm[0]).is_none());
    let tap = acts.iter().find(|a| a.ops.is_empty()).unwrap();
    assert!(p.retry_each(tap).is_none());
}

#[test]
fn plan_limited_versions_and_errors() {
    let p = BrewProvider::new();
    // plain formula: brew installs what it offers; the version stays on the op for drift reports.
    let acts = p
        .plan(&[op(OpKind::Install, Spec::new("jq"), Some("1.7.1"))])
        .unwrap();
    assert_eq!(argv(&acts[0]), "install jq");
    assert_eq!(acts[0].ops[0].version.as_deref(), Some("1.7.1"));
    // versioned formulae: the version must lie inside the formula's series.
    assert!(
        p.plan(&[op(
            OpKind::Install,
            Spec::new("python@3.12"),
            Some("3.12.4")
        )])
        .is_ok()
    );
    let e = p
        .plan(&[op(
            OpKind::Install,
            Spec::new("python@3.12"),
            Some("3.11.2"),
        )])
        .unwrap_err()
        .to_string();
    assert!(e.contains("3.12"), "{e}");
    // casks cannot be pinned; odd names and sources are rejected.
    assert!(
        p.plan(&[op(OpKind::Pin, brew::spec("x", true, None), None)])
            .is_err()
    );
    assert!(
        p.plan(&[op(OpKind::Install, Spec::new("--force"), None)])
            .is_err()
    );
    let bad = Spec::new("a").source("bogus");
    assert!(p.plan(&[op(OpKind::Install, bad, None)]).is_err());
}

// ---- exec ---------------------------------------------------------------------------------

#[test]
fn exec_runs_with_deterministic_env_and_logs_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, rec, ctx, b) = scripted(tmp.path());
    rec.on(
        &[&b, "install", "bad"],
        Output::fail(1, "Error: no formula\n"),
    );
    let ok = p
        .plan(&[op(OpKind::Install, Spec::new("a"), None)])
        .unwrap();
    let bad = p
        .plan(&[op(OpKind::Install, Spec::new("bad"), None)])
        .unwrap();

    let o1 = p.exec(&ctx, &ok[0]).unwrap();
    let o2 = p.exec(&ctx, &bad[0]).unwrap();
    assert!(o1.success && !o2.success);
    let (l1, l2) = (o1.log.unwrap(), o2.log.unwrap());
    assert_eq!(l1, ctx.log_dir.join("brew-1.log"));
    assert_eq!(l2, ctx.log_dir.join("brew-2.log"));
    assert!(std::fs::read_to_string(l2).unwrap().contains("no formula"));
    let cmds = rec.commands();
    assert_eq!(cmds[0].argv(), ok[0].argv);
    assert_eq!(cmds[0].env["HOMEBREW_NO_AUTO_UPDATE"], "1");
    assert_eq!(cmds[0].env["HOMEBREW_NO_INSTALL_CLEANUP"], "1");
    assert!(!cmds[0].env["PATH"].is_empty());

    let dry = Ctx {
        dry_run: true,
        ..ctx.clone()
    };
    rec.clear();
    assert!(p.exec(&dry, &ok[0]).unwrap().success);
    assert_eq!(rec.count(), 0);
}

// ---- bins / owner_of ----------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn bins_and_owner_of_use_one_query_and_the_filesystem() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, rec, ctx, b) = scripted(tmp.path());
    let prefix = tmp.path().join("prefix");
    let json = INSTALLED.replace("/opt/homebrew", &prefix.to_string_lossy());
    rec.on(&[&b, "info", "--json=v2", "--installed"], Output::ok(&json));

    let bat_bin = prefix.join("Cellar/bat/0.26.1/bin");
    std::fs::create_dir_all(&bat_bin).unwrap();
    std::fs::write(bat_bin.join("bat"), "").unwrap();
    std::fs::create_dir_all(prefix.join("Cellar/llvm/22.1.8/bin")).unwrap();
    std::fs::write(prefix.join("Cellar/llvm/22.1.8/bin/clang"), "").unwrap();
    let link = prefix.join("bin/bat");
    std::os::unix::fs::symlink("../Cellar/bat/0.26.1/bin/bat", &link).unwrap();

    let installed: BTreeMap<String, Installed> = p.installed(&ctx).unwrap();
    rec.clear();
    let bins = p.bins(&ctx, &installed).unwrap();
    assert_eq!(
        rec.count(),
        1,
        "bins is one batched call: {:?}",
        rec.lines()
    );
    let find = |pkg: &str, bin: &str| bins.iter().find(|e| e.package == pkg && e.bin == bin);
    assert_eq!(find("bat", "bat").unwrap().path, link);
    // keg-only formula: the keg path, since nothing is linked into the prefix.
    assert_eq!(
        find("llvm", "clang").unwrap().path,
        prefix.join("Cellar/llvm/22.1.8/bin/clang")
    );
    // cask `binary` artifacts that land in <prefix>/bin; completions are skipped.
    assert!(find("gcloud-cli", "gcloud").is_some());
    assert_eq!(
        find("gcloud-cli", "git-credential-gcloud").unwrap().path,
        prefix.join("bin/git-credential-gcloud")
    );
    assert!(find("aerospace", "aerospace").is_some());
    assert!(bins.iter().filter(|e| e.package == "aerospace").count() == 1);

    let stray = tmp.path().join("elsewhere");
    std::fs::write(&stray, "").unwrap();
    rec.clear();
    let owners = p.owner_of(&ctx, &[link.clone(), stray.clone()]).unwrap();
    assert_eq!(owners.get(&link).map(String::as_str), Some("bat"));
    assert!(!owners.contains_key(&stray));
    assert_eq!(rec.count(), 0);
}

// ---- availability: fresh Mac --------------------------------------------------------------

#[test]
fn fresh_mac_bootstraps_then_resolves_again_without_a_new_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let want = tmp.path().join("opt/bin/brew");
    let p = BrewProvider::with_candidates(vec![want.clone()]);
    let ctx = ctx_with(Arc::new(RecordingRunner::new()), tmp.path());

    // No brew yet: resolution can pick brew with a bootstrap package, or fall to another provider.
    assert_eq!(
        p.availability_in(&ctx, &facts("macos")),
        Availability::Bootstrap {
            pkg: "homebrew".into()
        }
    );
    assert_eq!(
        p.availability_in(&ctx, &facts("linux")),
        Availability::Bootstrap {
            pkg: "homebrew".into()
        }
    );
    assert_eq!(
        p.availability_in(&ctx, &facts("windows")),
        Availability::Unsupported
    );
    // The plan can be made before the bootstrap: it already names the final path layout.
    let planned = p
        .plan(&[op(OpKind::Install, Spec::new("jq"), None)])
        .unwrap();
    assert!(Path::new(&planned[0].argv[0]).is_absolute());

    // Homebrew gets installed during the run; the same provider sees it, PATH untouched.
    fake_prefix(&tmp.path().join("opt"));
    assert_eq!(
        p.availability_in(&ctx, &facts("macos")),
        Availability::Ready {
            bin_path: want.clone()
        }
    );
    // exec of the pre-bootstrap plan lands on the installed brew.
    let stale = dots_providers::Action {
        argv: vec![
            "/nonexistent/bin/brew".into(),
            "install".into(),
            "jq".into(),
        ],
        ..planned[0].clone()
    };
    let rec = Arc::new(RecordingRunner::new());
    let ctx = ctx_with(rec.clone(), tmp.path());
    assert!(p.exec(&ctx, &stale).unwrap().success);
    assert_eq!(rec.commands()[0].program, want);
}

#[test]
fn homebrew_prefix_from_env_is_honoured() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = fake_prefix(&tmp.path().join("custom"));
    let p = BrewProvider::with_candidates(vec![tmp.path().join("none/bin/brew")]);
    let mut ctx = ctx_with(Arc::new(RecordingRunner::new()), tmp.path());
    ctx.env.insert(
        "HOMEBREW_PREFIX".into(),
        tmp.path().join("custom").to_string_lossy().into_owned(),
    );
    assert_eq!(
        p.availability_in(&ctx, &facts("macos")),
        Availability::Ready { bin_path: bin }
    );
}

// ---- the real brew (read-only) ------------------------------------------------------------

#[cfg(target_os = "macos")]
#[test]
fn real_brew_read_only() {
    let p = BrewProvider::new();
    let mut ctx = Ctx::default();
    ctx.env.insert("PATH".into(), String::new());
    let Availability::Ready { bin_path } = p.availability_in(&ctx, &facts("macos")) else {
        eprintln!("no Homebrew on this machine; skipping");
        return;
    };
    assert!(bin_path.ends_with("bin/brew"));
    let rec = Arc::new(RecordingRunner::wrapping(Arc::new(RealRunner)));
    ctx.runner = rec.clone();
    let m = p.installed(&ctx).expect("brew info --json=v2 --installed");
    assert_eq!(rec.count(), 1);
    // brew never reports an empty version, and every key is a plain name.
    assert!(m.values().all(|i| !i.version.is_empty()));
    assert!(m.keys().all(|k| !k.is_empty()));
}

#[test]
fn roots_are_what_the_user_asked_for_not_what_brew_pulled_in() {
    let tmp = tempfile::tempdir().unwrap();
    let (p, rec, ctx, b) = scripted(tmp.path());
    rec.on(
        &[&b, "info", "--json=v2", "--installed"],
        Output::ok(INSTALLED),
    );
    let roots = p.roots(&ctx).unwrap().expect("brew can tell");
    assert_eq!(rec.count(), 1, "one batched call: {:?}", rec.lines());
    assert!(roots.contains("bat"));
    // Installed only as a dependency: not a root.
    assert!(!roots.contains("jpeg-turbo"));
    // Casks are always on request.
    assert!(roots.contains("1password-cli"));
}
