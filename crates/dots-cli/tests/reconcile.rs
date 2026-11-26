//! Tasks 4.1-4.3 through the real binary: `plan` and `apply` against script packages in a
//! `--home` sandbox (no package manager is touched).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Box_ {
    tmp: tempfile::TempDir,
    home: PathBuf,
    machine: PathBuf,
}

impl Box_ {
    fn state(&self) -> PathBuf {
        self.home.join(".local/state/dots")
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// `pkgs`: (name, extra package.toml keys, install command, requires).
fn sandbox(pkgs: &[(&str, &str, &str)]) -> Box_ {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for (name, requires, install) in pkgs {
        names.push(format!("\"{name}\""));
        write(
            &root.join(format!("packages/{name}/package.toml")),
            &format!(
                "schema = 1\nname = \"{name}\"\n{requires}\n[providers.script]\ncheck = \"test -f $HOME/{name}.installed\"\nversion = \"echo 1.0.0\"\ninstall = \"{install}\"\n"
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
    Box_ { tmp, home, machine }
}

fn touch(name: &str) -> String {
    format!("touch $HOME/{name}.installed")
}

fn cmd(b: &Box_, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dots"));
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
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

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(b: &Box_, args: &[&str]) -> Out {
    let o = cmd(b, args).stdin(Stdio::null()).output().unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", o.stdout))
}

fn validate(command: &str, doc: &Value) {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../schemas/dots-{command}.schema.json")),
        )
        .unwrap(),
    )
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(doc)
        .unwrap_or_else(|e| panic!("{e}\n{doc}"));
}

fn wait_for(p: &Path) {
    let t = Instant::now();
    while !p.exists() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "timed out waiting for {p:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn finish(mut c: Child) -> i32 {
    c.wait().unwrap().code().unwrap_or(-1)
}

#[test]
fn plan_shows_the_actions_without_side_effects_and_check_exits_3() {
    let b = sandbox(&[("hello", "", &touch("hello"))]);
    let o = run(&b, &["plan", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let d = json(&o);
    validate("plan", &d);
    assert_eq!(d["schema"], "dots/plan@1");
    assert_eq!(d["data"]["steps"][0]["package"], "hello");
    assert_eq!(d["data"]["steps"][0]["action"], "install");
    assert_eq!(d["data"]["summary"]["packages"], 1);
    assert!(!b.home.join("hello.installed").exists());
    assert!(
        !b.state().join("state.json").exists(),
        "plan writes nothing"
    );

    let o = run(&b, &["plan", "--check", "--color", "never"]);
    assert_eq!(o.code, 3, "{}", o.stderr);
    assert!(o.stdout.contains("1 install"), "{}", o.stdout);
    assert!(o.stdout.contains("+  hello  script"), "{}", o.stdout);
}

#[test]
fn apply_runs_the_plan_and_a_second_apply_does_nothing() {
    let b = sandbox(&[
        ("base", "", &touch("base")),
        ("app", "requires = [\"base\"]", &touch("app")),
    ]);
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let d = json(&o);
    validate("apply", &d);
    assert_eq!(d["data"]["status"], "done");
    assert_eq!(d["data"]["summary"]["ok"], 2);
    assert!(b.home.join("base.installed").exists() && b.home.join("app.installed").exists());
    // state and journal landed in the sandboxed state dir
    let st: Value =
        serde_json::from_str(&std::fs::read_to_string(b.state().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(st["schema"], 1);
    assert!(st["packages"]["base"].is_object() && st["packages"]["app"].is_object());
    let journal = std::fs::read_to_string(b.state().join("journal.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 2);
    let first: Value = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
    assert_eq!(first["op"], "install");
    assert_eq!(first["pkg"], "base");

    // Second apply: zero actions, plan --check is clean.
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0);
    let d = json(&o);
    assert_eq!(d["data"]["results"].as_array().unwrap().len(), 0);
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);
    assert_eq!(
        std::fs::read_to_string(b.state().join("journal.jsonl")).unwrap(),
        journal
    );
}

#[test]
fn apply_without_yes_fails_fast_when_it_cannot_ask() {
    let b = sandbox(&[("hello", "", &touch("hello"))]);
    let o = run(&b, &["apply", "--no-input", "--json"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
    let d = json(&o);
    validate("apply", &d);
    assert_eq!(d["errors"][0]["code"], "confirmation-required");
    assert!(d["errors"][0]["hint"].as_str().unwrap().contains("--yes"));
    assert!(!b.home.join("hello.installed").exists());
    // stdin is not a TTY here either: same result without the flag.
    assert_eq!(run(&b, &["apply"]).code, 1);
}

#[test]
fn apply_dry_run_changes_nothing() {
    let b = sandbox(&[("hello", "", &touch("hello"))]);
    let o = run(&b, &["apply", "--dry-run", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let d = json(&o);
    validate("apply", &d);
    assert_eq!(d["data"]["status"], "dry-run");
    assert!(!b.home.join("hello.installed").exists());
    assert!(
        !b.state().join("run.lock").exists(),
        "dry-run takes no lock"
    );
}

#[test]
fn one_failing_package_exits_1_and_the_rest_complete() {
    let b = sandbox(&[
        ("bad", "", "echo boom-from-bad >&2; exit 3"),
        ("child", "requires = [\"bad\"]", &touch("child")),
        ("good", "", &touch("good")),
    ]);
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
    let d = json(&o);
    validate("apply", &d);
    let status = |p: &str| {
        d["data"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["package"] == p)
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(status("bad"), "failed");
    assert_eq!(status("child"), "skipped-due-to-dep");
    assert_eq!(status("good"), "ok");
    assert!(b.home.join("good.installed").exists());
    assert!(!b.home.join("child.installed").exists());
    let codes: Vec<&str> = d["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"action-failed"), "{codes:?}");
}

#[test]
fn selective_apply_takes_the_requires_closure_only() {
    let b = sandbox(&[
        ("base", "", &touch("base")),
        ("app", "requires = [\"base\"]", &touch("app")),
        ("other", "", &touch("other")),
    ]);
    let o = run(&b, &["apply", "app", "--yes"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(b.home.join("base.installed").exists() && b.home.join("app.installed").exists());
    assert!(!b.home.join("other.installed").exists());
    let o = run(&b, &["apply", "nope", "--yes", "--json"]);
    assert_eq!(o.code, 1);
    assert_eq!(json(&o)["errors"][0]["code"], "unknown-package");
}

#[test]
fn a_second_concurrent_apply_exits_75_with_lock_busy() {
    let b = sandbox(&[(
        "slow",
        "",
        "touch $HOME/slow.started; sleep 3; touch $HOME/slow.installed",
    )]);
    let first = cmd(&b, &["apply", "--yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&b.home.join("slow.started"));
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 75, "{} {}", o.stdout, o.stderr);
    let d = json(&o);
    assert_eq!(d["errors"][0]["code"], "lock-busy");
    assert!(
        d["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains(&first.id().to_string())
    );
    // Read-only commands never take the lock.
    assert_eq!(run(&b, &["plan"]).code, 0);
    assert_eq!(finish(first), 0);
}

#[test]
fn sigint_stops_after_the_current_action_and_exits_130() {
    let b = sandbox(&[
        (
            "first",
            "",
            "touch $HOME/first.started; sleep 2; touch $HOME/first.installed",
        ),
        ("second", "requires = [\"first\"]", &touch("second")),
    ]);
    let first = cmd(&b, &["apply", "--yes"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&b.home.join("first.started"));
    let st = Command::new("kill")
        .args(["-INT", &first.id().to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(finish(first), 130);
    assert!(
        b.home.join("first.installed").exists(),
        "the running action finished"
    );
    assert!(
        !b.home.join("second.installed").exists(),
        "the next one never started"
    );
    let st: Value =
        serde_json::from_str(&std::fs::read_to_string(b.state().join("state.json")).unwrap())
            .unwrap();
    let owned: Vec<&String> = st["packages"].as_object().unwrap().keys().collect();
    assert_eq!(owned, vec!["first"]);
    // Re-running finishes the job.
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(b.home.join("second.installed").exists());
}
