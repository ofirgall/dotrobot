use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dots_providers::git::GitProvider;
use dots_providers::{Availability, Ctx, Op, OpKind, Provider, RealRunner, Spec};
use dots_testkit::RecordingRunner;
use dots_testkit::{ConformanceFixture, conformance_test};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(git_path().expect("git"))
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A local bare repo with two commits on `main`, the first tagged `v1.0`. Returns (bare path,
/// first commit sha, second commit sha).
fn bare_repo(root: &Path) -> (PathBuf, String, String) {
    let bare = root.join("origin.git");
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(root, &["init", "--bare", "-q", bare.to_str().unwrap()]);
    git(&work, &["init", "-q"]);
    git(&work, &["checkout", "-q", "-b", "main"]);
    std::fs::write(work.join("f"), "1").unwrap();
    git(&work, &["add", "f"]);
    git(&work, &["commit", "-q", "-m", "one"]);
    git(&work, &["tag", "v1.0"]);
    let first = git(&work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("f"), "2").unwrap();
    git(&work, &["commit", "-q", "-am", "two"]);
    let second = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &["push", "-q", bare.to_str().unwrap(), "main", "--tags"],
    );
    (bare, first, second)
}

/// The real git these tests drive: `/usr/bin/git`, or Git for Windows.
fn git_path() -> Option<PathBuf> {
    #[cfg(unix)]
    let cands = vec![PathBuf::from("/usr/bin/git")];
    #[cfg(windows)]
    let cands: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .flat_map(|d| {
            let d = PathBuf::from(d);
            [
                d.join("Git").join("cmd").join("git.exe"),
                d.join("Programs").join("Git").join("cmd").join("git.exe"),
            ]
        })
        .collect();
    cands.into_iter().find(|p| p.is_file())
}

fn have_git() -> bool {
    git_path().is_some()
}

conformance_test!(git_conforms, {
    let fx = ConformanceFixture::new("present", "absent")
        .exact_version("v1.0")
        .backend(Arc::new(RealRunner));
    let root = fx.sandbox_dir().to_path_buf();
    let (bare, _, _) = bare_repo(&root);
    let url = bare.to_str().unwrap();
    git(
        &root,
        &[
            "clone",
            "-q",
            "--branch",
            "v1.0",
            url,
            root.join("present").to_str().unwrap(),
        ],
    );
    let p = GitProvider::for_ctx(&fx.ctx())
        .repo("present", url, root.join("present"))
        .repo("absent", url, root.join("absent"));
    (p, fx)
});

#[test]
fn install_at_commit_and_upgrade_to_branch() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (bare, first, second) = bare_repo(tmp.path());
    let dest = tmp.path().join("clone");
    let ctx = Ctx {
        runner: Arc::new(RealRunner),
        home: tmp.path().join("home"),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let p = GitProvider::for_ctx(&ctx).repo("r", bare.to_str().unwrap(), &dest);
    let run = |kind, rev: &str| {
        let op = Op {
            kind,
            spec: Spec::new("r"),
            version: Some(rev.to_string()),
        };
        for a in p.plan(&[op]).unwrap() {
            assert!(p.exec(&ctx, &a).unwrap().success, "{:?}", a.argv);
        }
    };
    run(OpKind::Install, &first);
    assert_eq!(p.installed(&ctx).unwrap()["r"].version, "v1.0");
    assert_eq!(
        p.installed(&ctx).unwrap()["r"].source.as_deref(),
        Some(first.as_str())
    );
    run(OpKind::Upgrade, "main");
    assert_eq!(
        p.installed(&ctx).unwrap()["r"].source.as_deref(),
        Some(second.as_str())
    );
    run(OpKind::Upgrade, "v1.0");
    assert_eq!(p.installed(&ctx).unwrap()["r"].version, "v1.0");
}

#[test]
fn plan_shapes_and_unknown_repo() {
    let p = GitProvider::with_bin("/g/git").repo("r", "https://x/y.git", "/d");
    let o = |kind, v: Option<&str>| Op {
        kind,
        spec: Spec::new("r"),
        version: v.map(str::to_string),
    };
    assert_eq!(
        p.plan(&[o(OpKind::Install, None)]).unwrap()[0].argv,
        ["/g/git", "clone", "https://x/y.git", "/d"]
    );
    assert_eq!(
        p.plan(&[o(OpKind::Install, Some("v2"))]).unwrap()[0].argv,
        ["/g/git", "clone", "--branch", "v2", "https://x/y.git", "/d"]
    );
    assert_eq!(
        p.plan(&[o(OpKind::Install, Some("0123abc"))]).unwrap()[0].argv[0],
        dots_providers::script::platform_shell()
            .0
            .to_string_lossy()
            .as_ref()
    );
    assert!(
        p.plan(&[Op {
            kind: OpKind::Install,
            spec: Spec::new("unknown"),
            version: None
        }])
        .is_err()
    );
    assert!(
        p.plan(&[o(OpKind::Remove, None)])
            .unwrap_err()
            .to_string()
            .contains("cannot uninstall")
    );
}

#[test]
fn availability_resolves_own_binary() {
    let ctx = Ctx {
        home: PathBuf::from("/nonexistent-home"),
        ..Ctx::default()
    };
    let a = GitProvider::new().availability_in(&ctx, &Default::default());
    assert_eq!(
        matches!(a, Availability::Ready { .. }),
        have_git()
            || Path::new("/opt/homebrew/bin/git").exists()
            || Path::new("/usr/local/bin/git").exists()
    );
}

#[test]
fn installed_reads_annotated_packed_tag_without_processes() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (bare, _, second) = bare_repo(tmp.path());
    git(
        &tmp.path().join("work"),
        &["tag", "-a", "v2.0", "-m", "two"],
    );
    git(
        &tmp.path().join("work"),
        &["push", "-q", bare.to_str().unwrap(), "v2.0"],
    );
    let dest = tmp.path().join("clone");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
    );
    let rec = Arc::new(RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        ..Ctx::default()
    };
    let p = GitProvider::new().repo("r", bare.to_str().unwrap(), &dest);
    let m = p.installed(&ctx).unwrap();
    assert_eq!(rec.count(), 0);
    assert_eq!(m["r"].version, "v2.0");
    assert_eq!(m["r"].source.as_deref(), Some(second.as_str()));
    // Detached HEAD at the first commit: the lightweight tag.
    git(&dest, &["checkout", "-q", "--detach", "v1.0"]);
    assert_eq!(p.installed(&ctx).unwrap()["r"].version, "v1.0");
    // A clone that does not exist is simply absent.
    let q = GitProvider::new().repo("n", "x", tmp.path().join("nope"));
    assert!(q.installed(&ctx).unwrap().is_empty());
}

/// A plan made with `new()` (bare `git`) still executes through the resolved binary, in both
/// the direct and the `/bin/sh -c` forms.
#[test]
fn exec_resolves_bare_program() {
    if !have_git() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let rec = Arc::new(RecordingRunner::new());
    let ctx = Ctx {
        runner: rec.clone(),
        home: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        ..Ctx::default()
    };
    let p = GitProvider::new().repo("r", "https://x/y.git", tmp.path().join("d"));
    let o = |v: Option<&str>| Op {
        kind: OpKind::Install,
        spec: Spec::new("r"),
        version: v.map(str::to_string),
    };
    let direct = p.plan(&[o(None)]).unwrap().remove(0);
    assert_eq!(direct.argv[0], "git");
    assert!(p.exec(&ctx, &direct).unwrap().success);
    assert!(rec.commands()[0].program.is_absolute());
    rec.clear();
    let sh = p.plan(&[o(Some("0123abc"))]).unwrap().remove(0);
    assert!(p.exec(&ctx, &sh).unwrap().success);
    let c = &rec.commands()[0];
    assert_eq!(c.program, dots_providers::script::platform_shell().0);
    #[cfg(unix)]
    assert!(Path::new(&c.args[2]).is_absolute(), "{:?}", c.args);
    // On Windows the resolved git is quoted into the PowerShell text.
    #[cfg(windows)]
    assert!(!c.args.last().unwrap().contains("& 'git'"), "{:?}", c.args);
}
