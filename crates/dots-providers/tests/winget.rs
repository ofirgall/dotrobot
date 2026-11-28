//! The winget provider against a fake `winget` (any OS), plus a read-only run against the real
//! one on Windows.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dots_providers::winget::{ARGS_SEP, WingetProvider};
use dots_providers::{Availability, Cmd, Ctx, Op, OpKind, Output, Provider, Result, Runner, Spec};
use dots_testkit::{ConformanceFixture, RecordingRunner, conformance_test};

/// A tiny in-memory winget: `export`, `show`, `install`, `upgrade`, `uninstall`.
#[derive(Debug, Default)]
struct FakeWinget(Mutex<BTreeMap<String, String>>);

fn flag(cmd: &Cmd, name: &str) -> Option<String> {
    let i = cmd.args.iter().position(|a| a == name)?;
    cmd.args.get(i + 1).cloned()
}

impl Runner for FakeWinget {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut s = self.0.lock().unwrap();
        match cmd.args.first().map(String::as_str) {
            Some("export") => {
                let pkgs: Vec<String> = s
                    .iter()
                    .map(|(id, v)| {
                        format!("{{\"PackageIdentifier\":\"{id}\",\"Version\":\"{v}\"}}")
                    })
                    .collect();
                let text = format!("{{\"Sources\":[{{\"Packages\":[{}]}}]}}", pkgs.join(","));
                std::fs::write(flag(cmd, "-o").unwrap(), text).unwrap();
                Ok(Output::ok(""))
            }
            Some("show") => Ok(Output::ok("Found X\nVersion: 9.9.9\n")),
            Some("install" | "upgrade") => {
                let id = flag(cmd, "--id").unwrap();
                let v = flag(cmd, "--version").unwrap_or_else(|| "9.9.9".into());
                s.insert(id, v);
                Ok(Output::ok("Successfully installed\n"))
            }
            Some("uninstall") => {
                s.remove(&flag(cmd, "--id").unwrap());
                Ok(Output::ok("Successfully uninstalled\n"))
            }
            _ => Ok(Output::fail(2, "unexpected")),
        }
    }
}

conformance_test!(winget_conforms, {
    let fx = ConformanceFixture::new("Present.Pkg", "Absent.Pkg").exact_version("2.0.0");
    let bin = fx
        .sandbox_dir()
        .join("home/AppData/Local/Microsoft/WindowsApps/winget.exe");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "").unwrap();
    let fake = FakeWinget(Mutex::new(BTreeMap::from([(
        "Present.Pkg".to_string(),
        "1.0.0".to_string(),
    )])));
    let fx = fx.backend(Arc::new(fake));
    (WingetProvider::for_ctx(&fx.ctx()), fx)
});

fn op(kind: OpKind, id: &str, version: Option<&str>) -> Op {
    Op {
        kind,
        spec: Spec::new(id),
        version: version.map(str::to_string),
    }
}

fn argv(p: &WingetProvider, o: Op) -> Vec<String> {
    p.plan(&[o]).unwrap().remove(0).argv
}

#[test]
fn plan_uses_exact_silent_and_agreements() {
    let p = WingetProvider::with_bin("/w/winget");
    assert_eq!(
        argv(&p, op(OpKind::Install, "Git.Git", None)),
        [
            "/w/winget",
            "install",
            "--id",
            "Git.Git",
            "--exact",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity"
        ]
    );
    let up = argv(&p, op(OpKind::Upgrade, "Git.Git", Some("2.48.0")));
    assert_eq!(&up[..2], ["/w/winget", "upgrade"]);
    assert!(
        up.windows(2).any(|w| w == ["--version", "2.48.0"]),
        "{up:?}"
    );
    let rm = argv(&p, op(OpKind::Remove, "Git.Git", None));
    assert_eq!(&rm[..4], ["/w/winget", "uninstall", "--id", "Git.Git"]);
    assert!(rm.contains(&"--silent".to_string()) && rm.contains(&"--exact".to_string()));
    let pin = argv(&p, op(OpKind::Pin, "Git.Git", Some("2.47.1")));
    assert_eq!(&pin[1..3], ["pin", "add"]);
    assert_eq!(
        &argv(&p, op(OpKind::Unpin, "Git.Git", None))[1..3],
        ["pin", "remove"]
    );
}

#[test]
fn override_and_args_are_added() {
    let p = WingetProvider::with_bin("/w/winget");
    let spec = Spec::new("Microsoft.VisualStudio.2022.BuildTools")
        .opt(
            "override",
            "--wait --add Microsoft.VisualStudio.Workload.VCTools",
        )
        .opt("args", &format!("--location{ARGS_SEP}D:\\Tools"));
    let a = p
        .plan(&[Op {
            kind: OpKind::Install,
            spec,
            version: None,
        }])
        .unwrap()
        .remove(0)
        .argv;
    let i = a.iter().position(|x| x == "--override").unwrap();
    assert_eq!(
        a[i + 1],
        "--wait --add Microsoft.VisualStudio.Workload.VCTools"
    );
    assert_eq!(&a[a.len() - 2..], ["--location", "D:\\Tools"]);
}

/// "Already installed" and "no applicable upgrade" are success; any other failure is not.
#[test]
fn winget_exit_codes_for_nothing_to_do_are_success() {
    #[derive(Debug)]
    struct Status(i32);
    impl Runner for Status {
        fn run(&self, _: &Cmd) -> Result<Output> {
            Ok(Output::fail(self.0, "nothing to do"))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let run = |status: i32, kind: OpKind| {
        let ctx = Ctx {
            runner: Arc::new(Status(status)),
            log_dir: tmp.path().join("logs"),
            home: tmp.path().into(),
            ..Ctx::default()
        };
        let p = WingetProvider::with_bin("/w/winget");
        let a = p.plan(&[op(kind, "Git.Git", None)]).unwrap().remove(0);
        p.exec(&ctx, &a).unwrap().success
    };
    assert!(run(-1_978_335_189, OpKind::Upgrade), "0x8A15002B");
    assert!(run(-1_978_335_135, OpKind::Install), "0x8A150061");
    assert!(run(0, OpKind::Install));
    assert!(!run(1, OpKind::Install));
}

#[test]
fn latest_reads_winget_show_one_package_at_a_time() {
    let rec = Arc::new(RecordingRunner::new());
    rec.on(
        &["/w/winget", "show", "--id", "Git.Git"],
        Output::ok("Found Git [Git.Git]\nVersion: 2.48.0\n"),
    );
    let ctx = Ctx {
        runner: rec.clone(),
        ..Ctx::default()
    };
    let p = WingetProvider::with_bin("/w/winget");
    let m = p.latest(&ctx, &[Spec::new("Git.Git")]).unwrap();
    assert_eq!(m["Git.Git"].version, "2.48.0");
    assert!(rec.lines()[0].contains("--exact"), "{:?}", rec.lines());
    // Offline: no call at all.
    rec.clear();
    let offline = Ctx {
        offline: true,
        ..ctx
    };
    assert!(
        p.latest(&offline, &[Spec::new("Git.Git")])
            .unwrap()
            .is_empty()
    );
    assert_eq!(rec.count(), 0);
}

#[test]
fn availability_is_unsupported_without_winget() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = Ctx {
        home: tmp.path().into(),
        ..Ctx::default()
    };
    let facts = dots_facts::Facts::default();
    assert_eq!(
        WingetProvider::new().availability_in(&ctx, &facts),
        Availability::Unsupported
    );
}

/// The real winget, read-only: `list` and `show`, never an install.
#[cfg(windows)]
#[test]
fn real_winget_read_only() {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let ctx = Ctx {
        env: BTreeMap::from([("LOCALAPPDATA".to_string(), local)]),
        home: std::env::var_os("USERPROFILE").unwrap_or_default().into(),
        cache_dir: std::env::temp_dir(),
        ..Ctx::default()
    };
    let p = WingetProvider::for_ctx(&ctx);
    let Availability::Ready { .. } = p.availability_in(&ctx, &dots_facts::Facts::default()) else {
        eprintln!("no winget on this machine; skipping");
        return;
    };
    let installed = p.installed(&ctx).expect("winget export or list");
    for (id, i) in &installed {
        assert!(!id.contains(' ') && !i.version.is_empty(), "{id} {i:?}");
    }
    // Any package the machine has, or the well-known Git.Git.
    let id = installed
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "Git.Git".into());
    let m = p.latest(&ctx, &[Spec::new(&id)]).expect("winget show");
    if let Some(l) = m.get(&id) {
        assert!(!l.version.is_empty());
    }
}
