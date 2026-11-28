//! `dots apply` streams its progress: a line when each step starts and when it finishes, on
//! stderr (here a pipe, not a terminal), and the same lines in the live run log, while the run
//! is still going. Script packages in a `--home` sandbox; no package manager is touched.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

struct Sandbox {
    tmp: tempfile::TempDir,
    home: PathBuf,
    machine: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
}

/// `pkgs`: (name, install command). Each is a script package that checks for
/// `$HOME/<name>.installed`.
fn sandbox(pkgs: &[(&str, &str)]) -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for (name, install) in pkgs {
        names.push(format!("\"{name}\""));
        write(
            &root.join(format!("packages/{name}/package.toml")),
            &format!(
                "schema = 1\nname = \"{name}\"\n[providers.script]\ncheck = \"test -f $HOME/{name}.installed\"\nversion = \"echo 1.0.0\"\ninstall = \"{install}\"\n"
            ),
        );
    }
    write(
        &root.join("bundles/all.toml"),
        &format!("schema = 1\npackages = [{}]\n", names.join(", ")),
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n",
            root.display().to_string()
        ),
    );
    Sandbox { tmp, home, machine }
}

fn cmd(b: &Sandbox, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dots"));
    c.env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("DOTS_PROVIDERS", "script")
        .env("HOME", b.tmp.path().join("sentinel"))
        .current_dir(b.tmp.path())
        .arg("--home")
        .arg(&b.home)
        .arg("--machine-config")
        .arg(&b.machine)
        .args(args);
    c
}

/// The run log of the one run in the sandbox.
fn run_log(b: &Sandbox) -> PathBuf {
    let logs = b.home.join(".local/state/dots/logs");
    let run = std::fs::read_dir(logs).unwrap().next().unwrap().unwrap();
    run.path().join("run.log")
}

#[test]
fn progress_streams_to_a_pipe_while_a_step_is_still_running() {
    let b = sandbox(&[
        ("quick", "touch $HOME/quick.installed"),
        (
            "slow",
            "while [ ! -f $HOME/go ]; do sleep 0.05; done; touch $HOME/slow.installed",
        ),
    ]);
    let mut child = cmd(&b, &["apply", "--yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });

    // Lines arrive while `slow` is blocked: the run log path first, then both starts and the
    // finish of `quick`.
    let mut seen: Vec<String> = Vec::new();
    let t = Instant::now();
    while !seen.iter().any(|l| l.contains("[x] Installed quick")) {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "no streamed lines: {seen:?}"
        );
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(100)) {
            seen.push(l);
        }
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "the run is still going while its first lines are out"
    );
    assert!(seen[0].contains("run log:"), "{seen:?}");
    assert!(
        seen.iter()
            .any(|l| l.contains("[..] Installing slow (script)"))
    );
    assert!(
        !seen.iter().any(|l| l.contains("[x] Installed slow")),
        "{seen:?}"
    );
    // The live log already has them too: `tail -f` works from another terminal.
    let log = std::fs::read_to_string(run_log(&b)).unwrap();
    assert!(
        log.contains("[..] Installing slow (script)") && log.contains("[x] Installed quick"),
        "{log}"
    );
    assert!(!log.contains('\x1b'));

    std::fs::write(b.home.join("go"), "").unwrap();
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut out).unwrap();
    assert!(child.wait().unwrap().success());
    while let Ok(l) = rx.recv_timeout(Duration::from_millis(500)) {
        seen.push(l);
    }
    assert!(
        seen.iter().any(|l| l.contains("[x] Installed slow")),
        "{seen:?}"
    );
    // Every streamed line is plain ASCII text: no escapes or carriage returns off a tty.
    for l in &seen {
        assert!(!l.contains('\x1b') && !l.contains('\r'), "{l:?}");
    }
    // The final summary is still there, on stdout, and does not repeat the stream.
    assert!(out.contains("quick") && out.contains("slow"), "{out}");
    assert!(!out.contains("run log:"));
    let log = std::fs::read_to_string(run_log(&b)).unwrap();
    assert!(log.contains("[x] Installed slow"), "{log}");
}

#[test]
fn json_keeps_stdout_one_document_and_streams_to_stderr() {
    let b = sandbox(&[("one", "touch $HOME/one.installed")]);
    let o = cmd(&b, &["apply", "--yes", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success());
    let doc: Value = serde_json::from_slice(&o.stdout).expect("stdout is one JSON document");
    assert_eq!(doc["data"]["status"], "done");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("run log:")
            && err.contains("[..] Installing one (script)")
            && err.contains("[x] Installed one")
    );
}

#[test]
fn quiet_streams_nothing_but_still_writes_the_log() {
    let b = sandbox(&[("one", "touch $HOME/one.installed")]);
    let o = cmd(&b, &["apply", "--yes", "--quiet"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stderr), "");
    let log = std::fs::read_to_string(run_log(&b)).unwrap();
    assert!(log.contains("[x] Installed one"), "{log}");
}

#[test]
fn a_failure_prints_its_error_and_log_path_right_away() {
    let b = sandbox(&[("bad", "echo boom-from-bad >&2; exit 3")]);
    let o = cmd(&b, &["apply", "--yes"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    let fail = err
        .lines()
        .position(|l| l.contains("[FAIL] Installing bad (script)"))
        .unwrap_or_else(|| panic!("{err}"));
    let rest: Vec<&str> = err.lines().skip(fail + 1).collect();
    assert!(rest.iter().any(|l| l.contains("bad:")), "{err}");
    assert!(rest.iter().any(|l| l.contains("boom-from-bad")), "{err}");
    assert!(
        rest.iter()
            .any(|l| l.contains("log: ") && l.contains("bad.install.log")),
        "{err}"
    );
}

#[test]
fn a_long_step_prints_still_running_lines_off_a_tty_and_into_the_log() {
    let b = sandbox(&[(
        "slow",
        "while [ ! -f $HOME/go ]; do sleep 0.05; done; touch $HOME/slow.installed",
    )]);
    let mut child = cmd(&b, &["apply", "--yes"])
        .env("DOTS_HEARTBEAT_SECS", "0.3")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut beats = 0;
    let t = Instant::now();
    let mut seen = Vec::new();
    while beats < 2 {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "no heartbeat lines: {seen:?}"
        );
        if let Ok(l) = rx.recv_timeout(Duration::from_millis(100)) {
            if l.contains("still running: Installing slow (script)") {
                assert!(l.starts_with(" [..] ") && l.ends_with(')'), "{l:?}");
                beats += 1;
            }
            seen.push(l);
        }
    }
    let log = std::fs::read_to_string(run_log(&b)).unwrap();
    assert!(
        log.contains("still running: Installing slow (script)"),
        "{log}"
    );
    std::fs::write(b.home.join("go"), "").unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_quick_step_has_no_heartbeat() {
    let b = sandbox(&[("one", "touch $HOME/one.installed")]);
    let o = cmd(&b, &["apply", "--yes"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("still running"));
}

#[test]
fn plan_json_stdout_is_one_document_and_stderr_has_no_progress_off_a_tty() {
    let b = sandbox(&[("one", "touch $HOME/one.installed")]);
    for args in [&["plan", "--json"][..], &["plan"]] {
        let o = cmd(&b, args).stdin(Stdio::null()).output().unwrap();
        assert!(o.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!err.contains('\x1b') && !err.contains('\r'), "{err:?}");
    }
    let o = cmd(&b, &["plan", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&o.stdout).expect("stdout is one JSON document");
    assert!(doc.get("data").is_some());
}
