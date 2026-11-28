//! Manual actions (`packages/<pkg>/manual/*.md`) and pending hooks through the real binary, in a
//! `--home` sandbox with script packages: `dots todo`, the checks, revisions, the `status`,
//! `plan` and `apply` summaries, `lint`, and the `post_install` hook of an adopted package.
//!
//! A script package is "installed" while `$HOME/<name>.ver` exists.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))] // helpers of unix-only tests

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    machine: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
}

/// A package `k` (script provider, installed once `$HOME/k.ver` exists) with `extra` keys.
fn sandbox(extra: &str) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    write(
        &root.join("packages/k/package.toml"),
        &format!(
            "schema = 1\nname = \"k\"\n{extra}\n[providers.script]\ncheck = \"test -f $HOME/k.ver\"\nversion = \"cat $HOME/k.ver\"\ninstall = \"echo 1.0.0 > $HOME/k.ver\"\n"
        ),
    );
    write(
        &root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"k\"]\n",
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
    Sb {
        tmp,
        home,
        root,
        machine,
    }
}

struct Out {
    stdout: String,
    stderr: String,
}

fn run(b: &Sb, args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("DOTS_PROVIDERS", "script")
        .env("HOME", b.tmp.path().join("sentinel"))
        .current_dir(b.tmp.path())
        .arg("--home")
        .arg(&b.home)
        .arg("--machine-config")
        .arg(&b.machine)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}\n{}", o.stdout, o.stderr))
}

fn manual(b: &Sb, name: &str, front: &str) {
    write(
        &b.root.join(format!("packages/k/manual/{name}.md")),
        &format!("---\n{front}\n---\n\nStep one: do the thing.\n"),
    );
}

fn install_k(b: &Sb) {
    write(&b.home.join("k.ver"), "1.0.0\n");
}

fn statuses(doc: &Value) -> Vec<(String, String)> {
    doc["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["id"].as_str().unwrap().to_string(),
                a["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn todo(b: &Sb) -> Value {
    json(&run(b, &["todo", "--json"]))
}

fn state(b: &Sb) -> Value {
    let text = std::fs::read_to_string(b.home.join(".local/state/dots/state.json"))
        .unwrap_or_else(|_| "{}".into());
    serde_json::from_str(&text).unwrap()
}

#[test]
fn a_manual_file_is_discovered_once_its_package_is_installed() {
    let b = sandbox("");
    manual(&b, "perm", "title: Grant access\nrequires: [logout]");
    // `after: install` (the default): nothing to do while k is not installed.
    assert!(statuses(&todo(&b)).is_empty());
    install_k(&b);
    let doc = todo(&b);
    assert_eq!(
        statuses(&doc),
        [("k/perm".to_string(), "pending".to_string())]
    );
    assert_eq!(doc["data"]["actions"][0]["requires"][0], "logout");
    let human = run(&b, &["todo"]).stdout;
    assert!(
        human.contains("1 manual action pending, 1 requires logout"),
        "{human}"
    );
    assert!(human.contains("k/perm"), "{human}");
}

#[test]
fn done_is_per_machine_and_a_new_revision_reopens_it() {
    let b = sandbox("");
    install_k(&b);
    manual(&b, "perm", "title: Grant access\nrevision: 1");
    assert_eq!(statuses(&todo(&b))[0].1, "pending");
    let o = run(&b, &["todo", "done", "k/perm", "--note", "did it"]);
    assert!(o.stdout.contains("k/perm marked done"), "{}", o.stdout);
    assert_eq!(statuses(&todo(&b))[0].1, "done");
    let rec = &state(&b)["manual"]["k/perm"];
    assert_eq!(rec["status"], "done");
    assert_eq!(rec["note"], "did it");
    assert_eq!(rec["revision"], 1);
    // The steps changed: pending again.
    manual(&b, "perm", "title: Grant access\nrevision: 2");
    assert_eq!(statuses(&todo(&b))[0].1, "pending");
}

#[test]
fn skip_hides_it_until_the_revision_increases() {
    let b = sandbox("");
    install_k(&b);
    manual(&b, "perm", "title: Grant access");
    run(&b, &["todo", "skip", "pkg:k/manual:perm"]);
    assert_eq!(statuses(&todo(&b))[0].1, "skipped");
    let human = run(&b, &["todo"]).stdout;
    assert!(human.contains("nothing to do by hand"), "{human}");
    manual(&b, "perm", "title: Grant access\nrevision: 2");
    assert_eq!(statuses(&todo(&b))[0].1, "pending");
}

#[test]
fn a_check_closes_the_action_as_auto() {
    let b = sandbox("");
    install_k(&b);
    manual(
        &b,
        "perm",
        "title: Grant access\ncheck: 'test -f $HOME/granted'",
    );
    let o = json(&run(&b, &["todo", "check", "--json"]));
    assert_eq!(o["data"]["not_yet"][0], "k/perm");
    assert_eq!(statuses(&todo(&b))[0].1, "pending");
    write(&b.home.join("granted"), "");
    let o = json(&run(&b, &["todo", "check", "--json"]));
    assert_eq!(o["data"]["closed"][0], "k/perm");
    assert_eq!(statuses(&todo(&b))[0].1, "auto");
}

#[test]
fn an_always_action_opens_again_when_its_check_stops_passing() {
    let b = sandbox("");
    install_k(&b);
    manual(
        &b,
        "grp",
        "title: Join the group\nafter: always\ncheck: 'test -f $HOME/member'",
    );
    write(&b.home.join("member"), "");
    run(&b, &["todo", "check"]);
    assert_eq!(statuses(&todo(&b))[0].1, "auto");
    std::fs::remove_file(b.home.join("member")).unwrap();
    let o = json(&run(&b, &["todo", "check", "--json"]));
    assert_eq!(o["data"]["reopened"][0], "k/grp");
    assert_eq!(statuses(&todo(&b))[0].1, "pending");
}

#[test]
fn show_prints_the_markdown_and_unknown_ids_are_errors() {
    let b = sandbox("");
    install_k(&b);
    manual(&b, "perm", "title: Grant access");
    let o = run(&b, &["todo", "show", "k/perm"]);
    assert!(o.stdout.contains("Step one: do the thing."), "{}", o.stdout);
    assert!(o.stdout.contains("dots todo done k/perm"), "{}", o.stdout);
    let bad = run(&b, &["todo", "show", "k/nope"]);
    assert!(bad.stderr.contains("manual-unknown"), "{}", bad.stderr);
}

#[test]
fn status_and_plan_report_pending_manual_actions() {
    let b = sandbox("");
    install_k(&b);
    manual(&b, "perm", "title: Grant access\nrequires: [reboot]");
    let st = json(&run(&b, &["status", "--json"]));
    let todos: Vec<&Value> = st["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["area"] == "todos")
        .collect();
    assert_eq!(todos.len(), 1, "{st}");
    assert_eq!(todos[0]["name"], "k/perm");
    assert_eq!(todos[0]["state"], "pending");
    let plan = run(&b, &["plan"]);
    assert!(
        plan.stdout
            .contains("1 manual action pending, 1 requires reboot"),
        "{}",
        plan.stdout
    );
    let plan = json(&run(&b, &["plan", "--json"]));
    assert_eq!(plan["data"]["manual"]["count"], 1);
}

#[test]
fn apply_runs_the_checks_and_ends_with_the_summary() {
    let b = sandbox("");
    manual(
        &b,
        "perm",
        "title: Grant access\ncheck: 'test -f $HOME/granted'\nrequires: [logout]",
    );
    manual(&b, "other", "title: Click the button\nrequires: [logout]");
    write(&b.home.join("granted"), "");
    let o = run(&b, &["apply", "--yes", "--no-input"]);
    // The install went through, the check passed, the other one is still open.
    assert!(
        o.stdout
            .contains("1 manual action pending, 1 requires logout"),
        "{}\n{}",
        o.stdout,
        o.stderr
    );
    assert!(o.stdout.contains("k/perm"), "{}", o.stdout);
    let s = state(&b);
    assert_eq!(s["manual"]["k/perm"]["status"], "auto");
    assert!(s["manual"].get("k/other").is_none());
}

#[test]
fn lint_validates_manual_files() {
    let b = sandbox("");
    manual(&b, "ok", "title: Fine");
    assert!(run(&b, &["lint"]).stdout.contains("lint: ok"));
    // Unknown key, bad `after`, duplicate id, and a `when` that does not parse.
    manual(&b, "bad", "title: Bad\nbogus: 1");
    manual(&b, "dup-a", "id: same\ntitle: A");
    manual(&b, "dup-b", "id: same\ntitle: B");
    let doc = json(&run(&b, &["lint", "--json"]));
    let codes: Vec<&str> = doc["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"manual-invalid"), "{codes:?}");
    assert!(codes.contains(&"manual-duplicate-id"), "{codes:?}");
}

#[test]
fn lint_checks_the_when_of_a_manual_file() {
    let b = sandbox("");
    manual(&b, "cond", "title: C\nwhen: 'os ='");
    let doc = json(&run(&b, &["lint", "--json"]));
    let errors = doc["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{doc}");
    assert!(
        errors[0]["provenance"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with("manual/cond.md"),
        "{doc}"
    );
}

// ------------------------------------------------------------------ pending hooks

const HOOK: &str = "[hooks]\npost_install = \"echo setup >> $HOME/hook.log\"\n";

fn hook_runs(b: &Sb) -> usize {
    std::fs::read_to_string(b.home.join("hook.log"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn an_adopted_package_runs_its_post_install_hook_once() {
    let b = sandbox(HOOK);
    // Already installed outside dots: adopted, no install step.
    install_k(&b);
    let plan = run(&b, &["plan"]);
    assert!(
        plan.stdout.contains("adopted, never ran"),
        "{}",
        plan.stdout
    );
    let o = run(&b, &["apply", "--yes", "--no-input"]);
    assert_eq!(hook_runs(&b), 1, "{}\n{}", o.stdout, o.stderr);
    assert!(state(&b).get("hooks_pending").is_none());
    // Nothing left to do on the next run.
    run(&b, &["apply", "--yes", "--no-input"]);
    assert_eq!(hook_runs(&b), 1);
    let plan = json(&run(&b, &["plan", "--json"]));
    assert_eq!(plan["data"]["hooks"].as_array().unwrap().len(), 0);
}

#[test]
fn status_reports_a_hook_that_still_has_to_run() {
    let b = sandbox("[hooks]\npost_install = \"exit 3\"\n");
    install_k(&b);
    // The hook fails: it stays owed, and `status` says so.
    run(&b, &["apply", "--yes", "--no-input"]);
    let st = json(&run(&b, &["status", "--json"]));
    let rows: Vec<&Value> = st["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["name"] == "k:post_install")
        .collect();
    assert_eq!(rows.len(), 1, "{st}");
    assert_eq!(rows[0]["state"], "pending");
}
