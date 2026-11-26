//! Task 1.6: `--home` keeps everything inside it; task 1.7: golden smoke tests.

mod common;

use common::{golden, stub_invocations};

fn names(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn home_flag_writes_nothing_outside_it() {
    let bin = env!("CARGO_BIN_EXE_dots");
    let tmp = tempfile::tempdir().unwrap();
    let (cwd, real_home, h) = (
        tmp.path().join("cwd"),
        tmp.path().join("real-home"),
        tmp.path().join("h"),
    );
    for d in [&cwd, &real_home, &h] {
        std::fs::create_dir_all(d).unwrap();
    }
    for inv in stub_invocations() {
        let status = std::process::Command::new(bin)
            .env_clear()
            .env("HOME", &real_home)
            // Env redirection must lose against the explicit flag.
            .env("DOTS_STATE_DIR", tmp.path().join("env-state"))
            .env("XDG_CACHE_HOME", tmp.path().join("env-cache"))
            .current_dir(&cwd)
            .arg("--home")
            .arg(&h)
            .args(*inv)
            .output()
            .unwrap()
            .status;
        assert_eq!(status.code(), Some(1));
    }
    assert_eq!(names(tmp.path()), ["cwd", "h", "real-home"]);
    assert!(names(&cwd).is_empty() && names(&real_home).is_empty());
}

#[test]
fn smoke_golden_help() {
    golden().snapshot("help", &["--help"]);
}

#[test]
fn golden_not_implemented_stub() {
    golden().snapshot("roots-stub", &["roots"]);
}
