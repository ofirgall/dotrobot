//! Links on a Windows account that may not create symlinks: a hardlink for a file, a junction
//! for a directory, recorded in the ledger, with status and removal that understand them.
//! Each test refuses symlinks first (a developer machine can usually create them), which is
//! why they live in a binary of their own.
#![cfg(windows)]

use std::fs;
use std::path::PathBuf;

use dots_files::*;

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    root: PathBuf,
}

fn env() -> Env {
    refuse_symlinks();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let state = home.join(".local").join("state").join("dots");
    let root = tmp.path().join("dotfiles");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&root).unwrap();
    Env {
        _tmp: tmp,
        home,
        state,
        root,
    }
}

impl Env {
    fn ctx(&self, run: &str) -> FilesCtx {
        let mut c = FilesCtx::new(&self.home, &self.state, &self.root, "root1", run);
        c.os = "windows".into();
        c.chain = vec!["any".into(), "windows".into()];
        c
    }
    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }
    fn file(
        &self,
        pkg: &str,
        mode: Mode,
        dst: &str,
        src: Option<PathBuf>,
        layers: &[&str],
    ) -> ResolvedFile {
        ResolvedFile {
            package: pkg.into(),
            mode,
            dst: self.home.join(dst),
            src,
            layers: layers
                .iter()
                .map(|l| LayerSrc {
                    path: self.root.join(l),
                    label: l.to_string(),
                })
                .collect(),
            format: "text".into(),
            kind: None,
            template: None,
            merge: Default::default(),
            comment: "#".into(),
            extras: Default::default(),
        }
    }
}

fn apply_all(files: &[ResolvedFile], ctx: &FilesCtx) -> Vec<FileOutcome> {
    plan(files, ctx).iter().map(|s| apply(s, ctx)).collect()
}

#[test]
fn a_file_link_becomes_a_hardlink_and_is_recorded() {
    let e = env();
    let src = e.write("packages/app/config", "a = 1\n");
    let f = e.file(
        "app",
        Mode::Link,
        ".config/app/config",
        Some(src.clone()),
        &[],
    );
    let ctx = e.ctx("r1");
    let out = apply_all(&[f.clone()], &ctx);
    assert_eq!(out[0].status, OutcomeStatus::Applied, "{out:?}");
    // A regular file with the target's content, not a symlink.
    let meta = fs::symlink_metadata(&f.dst).unwrap();
    assert!(meta.is_file() && !meta.file_type().is_symlink());
    assert_eq!(fs::read_to_string(&f.dst).unwrap(), "a = 1\n");
    let led = Ledger::load(&e.state);
    assert_eq!(
        led.owned.values().next().unwrap().link_kind,
        LinkKind::Hardlink
    );
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    assert_eq!(plan(&[f.clone()], &ctx)[0].action, Action::Unchanged);
    // An edit through the link shows as the target changing too: still ok.
    fs::write(&src, "a = 2\n").unwrap();
    assert_eq!(fs::read_to_string(&f.dst).unwrap(), "a = 2\n");
    // A write that replaces the target by rename leaves the hardlink on the old file.
    atomic_write(&src, b"a = 3\n").unwrap();
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Modified);
}

#[test]
fn a_directory_link_becomes_a_junction() {
    let e = env();
    e.write("packages/app/dir/one.txt", "1");
    let src = e.root.join("packages").join("app").join("dir");
    let f = e.file("app", Mode::Link, ".config/appdir", Some(src), &[]);
    let ctx = e.ctx("r1");
    let out = apply_all(&[f.clone()], &ctx);
    assert_eq!(out[0].status, OutcomeStatus::Applied, "{out:?}");
    assert_eq!(fs::read_to_string(f.dst.join("one.txt")).unwrap(), "1");
    let led = Ledger::load(&e.state);
    assert!(
        led.owned
            .values()
            .any(|r| r.link_kind == LinkKind::Junction)
    );
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
}

#[test]
fn a_rendered_file_follows_its_generated_copy_and_removal_cleans_up() {
    let e = env();
    e.write("packages/app/files/conf", "x = 1\n");
    let f = e.file(
        "app",
        Mode::Render,
        ".conf",
        None,
        &["packages/app/files/conf"],
    );
    let ctx = e.ctx("r1");
    let out = apply_all(&[f.clone()], &ctx);
    assert_eq!(out[0].status, OutcomeStatus::Applied, "{out:?}");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Ok);
    // The source changes: the generated copy is rewritten by rename, so the hardlink is stale
    // until the next apply remakes it.
    e.write("packages/app/files/conf", "x = 2\n");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Stale);
    let ctx2 = e.ctx("r2");
    let out = apply_all(&[f.clone()], &ctx2);
    assert_eq!(out[0].status, OutcomeStatus::Applied, "{out:?}");
    assert!(fs::read_to_string(&f.dst).unwrap().ends_with("x = 2\n"));
    assert_eq!(status(&[f.clone()], &ctx2)[0].state, FileState::Ok);
    // Nothing desired any more: the hardlink and the generated copy go.
    let rm = plan_removals(&[], &ctx2, None);
    assert_eq!(rm.len(), 1);
    let out = apply(&rm[0], &ctx2);
    assert_eq!(out.status, OutcomeStatus::Applied, "{out:?}");
    assert!(!f.dst.exists());
}
