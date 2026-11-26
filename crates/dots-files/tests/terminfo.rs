#![allow(clippy::cloned_ref_to_slice_refs)]
//! The `terminfo` mode against a fake `tic` and `infocmp` (no ncurses needed).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dots_files::terminfo::{ToolOutput, Tools};
use dots_files::*;
use dots_model::FileEntry;
use serde_json::json;

const SRC: &str = "herdr-256color|herdr with undercurl,\n\tam, Smulx=\\E[4:%p1%dm,\n";

/// `tic` stores the source text at `<dir>/<first letter>/<name>`; `infocmp` prints it back
/// behind a `#` header naming the directory.
#[derive(Debug, Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

impl Tools for Fake {
    fn run(&self, program: &str, args: &[String]) -> io::Result<ToolOutput> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("{program} {}", args.join(" ")));
        let ok = |stdout: String| ToolOutput {
            status: 0,
            stdout,
            stderr: String::new(),
        };
        let fail = || ToolOutput {
            status: 1,
            stdout: String::new(),
            stderr: "boom".into(),
        };
        Ok(match program {
            "tic" => {
                // -x -o <dir> <src>
                let (dir, src) = (Path::new(&args[2]), Path::new(&args[3]));
                let text = fs::read_to_string(src)?;
                let name = text.split(['|', ',']).next().unwrap().to_string();
                let d = dir.join(&name[..1]);
                fs::create_dir_all(&d)?;
                fs::write(d.join(&name), text)?;
                ok(String::new())
            }
            "infocmp" => {
                // -x -A <dir> <name>
                let (dir, name) = (Path::new(&args[2]), &args[3]);
                match fs::read_to_string(dir.join(&name[..1]).join(name)) {
                    Ok(t) => ok(format!(
                        "#\tReconstructed via infocmp from file: {}\n{t}",
                        dir.display()
                    )),
                    Err(_) => fail(),
                }
            }
            _ => fail(),
        })
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    root: PathBuf,
    tools: Arc<Fake>,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("dotfiles");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(root.join("herdr")).unwrap();
    fs::write(root.join("herdr/herdr-256color.terminfo"), SRC).unwrap();
    Env {
        state: home.join(".local/state/dots"),
        home,
        root,
        _tmp: tmp,
        tools: Arc::new(Fake::default()),
    }
}

impl Env {
    fn ctx(&self, run: &str) -> FilesCtx {
        let mut c = FilesCtx::new(&self.home, &self.state, &self.root, "root1", run);
        c.os = "macos".into();
        c.chain = vec!["any".into(), "unix".into(), "macos".into()];
        c.tools = self.tools.clone();
        c
    }
    fn resolved(&self) -> ResolvedFile {
        let entry: FileEntry =
            serde_json::from_value(json!({ "mode": "terminfo", "src": "herdr-256color.terminfo" }))
                .unwrap();
        resolve_entry(
            "herdr",
            &self.root.join("herdr"),
            &entry,
            &self.ctx("r"),
            &|_| Ok(true),
            &[],
        )
        .unwrap()
        .unwrap()
    }
}

#[test]
fn compile_on_apply() {
    let e = env();
    let f = e.resolved();
    assert_eq!(f.mode, Mode::Terminfo);
    assert_eq!(f.dst, e.home.join(".terminfo/herdr-256color"));
    let ctx = e.ctx("r1");
    let steps = plan(&[f.clone()], &ctx);
    assert_eq!(steps[0].action, Action::Create);
    assert_eq!(steps[0].from, Some(FileState::Missing));
    let out = apply(&steps[0], &ctx);
    assert_eq!(out.status, OutcomeStatus::Applied, "{:?}", out.error);
    let calls = e.tools.calls.lock().unwrap().clone();
    let tic = format!(
        "tic -x -o {} {}",
        e.home.join(".terminfo").display(),
        e.root.join("herdr/herdr-256color.terminfo").display()
    );
    assert!(calls.contains(&tic), "{calls:?}");
    assert!(e.home.join(".terminfo/h/herdr-256color").is_file());
}

#[test]
fn already_installed_runs_nothing_for_real() {
    let e = env();
    let f = e.resolved();
    let ctx = e.ctx("r1");
    apply_all(&[f.clone()], &ctx);
    let before = e.tools.calls.lock().unwrap().len();
    let ctx2 = e.ctx("r2");
    assert_eq!(status(&[f.clone()], &ctx2)[0].state, FileState::Ok);
    let steps = plan(&[f], &ctx2);
    assert_eq!(steps[0].action, Action::Unchanged);
    assert_eq!(apply(&steps[0], &ctx2).status, OutcomeStatus::Unchanged);
    // Only scratch compiles (into the state dir) for the comparison; nothing into ~/.terminfo.
    let calls = e.tools.calls.lock().unwrap();
    let real = format!("-o {} ", e.home.join(".terminfo").display());
    assert!(
        calls[before..].iter().all(|c| !c.contains(&real)),
        "{calls:?}"
    );
}

#[test]
fn source_changed_recompiles() {
    let e = env();
    let f = e.resolved();
    apply_all(&[f.clone()], &e.ctx("r1"));
    fs::write(
        e.root.join("herdr/herdr-256color.terminfo"),
        format!("{SRC}\tbce,\n"),
    )
    .unwrap();
    let ctx = e.ctx("r2");
    assert_eq!(status(&[f.clone()], &ctx)[0].state, FileState::Stale);
    let steps = plan(&[f.clone()], &ctx);
    assert_eq!(steps[0].action, Action::Update);
    assert_eq!(apply(&steps[0], &ctx).status, OutcomeStatus::Applied);
    assert_eq!(status(&[f], &e.ctx("r3"))[0].state, FileState::Ok);
}

#[test]
fn tic_failure_is_reported() {
    let e = env();
    fs::write(e.root.join("herdr/herdr-256color.terminfo"), SRC).unwrap();
    #[derive(Debug)]
    struct Broken;
    impl Tools for Broken {
        fn run(&self, _: &str, _: &[String]) -> io::Result<ToolOutput> {
            Ok(ToolOutput {
                status: 1,
                stdout: String::new(),
                stderr: "bad source".into(),
            })
        }
    }
    let mut ctx = e.ctx("r1");
    ctx.tools = Arc::new(Broken);
    let f = e.resolved();
    let out = apply_all(&[f], &ctx);
    assert_eq!(out[0].status, OutcomeStatus::Failed);
    assert!(
        out[0]
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("bad source")
    );
}

fn apply_all(files: &[ResolvedFile], ctx: &FilesCtx) -> Vec<FileOutcome> {
    plan(files, ctx).iter().map(|s| apply(s, ctx)).collect()
}
