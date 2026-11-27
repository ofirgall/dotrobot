//! Task 5.9 through the real binary: `dots.lock`, `upgrade`, `pin`/`unpin` and `lock --fix`
//! in a `--home` sandbox with script packages (no package manager is touched).
//!
//! A script package keeps its version in `$HOME/<name>.ver` and what its "provider" offers in
//! `$HOME/<name>.latest`, so a test sets both and `install` / `upgrade` copy one to the other.

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
    std::fs::write(p, text).unwrap();
}

/// `(name, extra keys before the provider table)`.
fn script(name: &str, extra: &str) -> (String, String) {
    (
        name.to_string(),
        format!(
            "{extra}\n[providers.script]\ncheck = \"test -f $HOME/{name}.ver\"\nversion = \"cat $HOME/{name}.ver\"\nlatest = \"cat $HOME/{name}.latest\"\ninstall = \"cp $HOME/{name}.latest $HOME/{name}.ver\"\nupgrade = \"cp $HOME/{name}.latest $HOME/{name}.ver\"\n"
        ),
    )
}

fn sandbox(pkgs: &[(String, String)]) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for (name, body) in pkgs {
        names.push(format!("\"{name}\""));
        write(
            &root.join(format!("packages/{name}/package.toml")),
            &format!("schema = 1\nname = \"{name}\"\n{body}"),
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
    code: i32,
}

fn run(b: &Sb, args: &[&str]) -> Out {
    run_env(b, &[], args)
}

fn run_env(b: &Sb, env: &[(&str, &str)], args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("DOTS_PROVIDERS", "script")
        .env("HOME", b.tmp.path().join("sentinel"))
        .envs(env.iter().copied())
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
        code: o.status.code().unwrap_or(-1),
    }
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}\n{}", o.stdout, o.stderr))
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

fn set(b: &Sb, name: &str, ver: Option<&str>, latest: &str) {
    if let Some(v) = ver {
        write(&b.home.join(format!("{name}.ver")), &format!("{v}\n"));
    }
    write(
        &b.home.join(format!("{name}.latest")),
        &format!("{latest}\n"),
    );
}

fn ver(b: &Sb, name: &str) -> String {
    std::fs::read_to_string(b.home.join(format!("{name}.ver")))
        .unwrap()
        .trim()
        .to_string()
}

fn lock_text(b: &Sb) -> String {
    std::fs::read_to_string(b.root.join("dots.lock")).unwrap_or_default()
}

/// This machine's platform key, as `dots facts` reports it.
fn machine_key(b: &Sb) -> String {
    let d = json(&run(b, &["facts", "--json"]));
    let chain = d["data"]["platform_chain"].as_array().unwrap();
    format!(
        "{}/{}",
        chain.last().unwrap().as_str().unwrap(),
        d["data"]["arch"].as_str().unwrap()
    )
}

fn seed_lock(b: &Sb, rows: &[(&str, &str, &str)]) {
    // (name, platform, version): every row is a script row.
    let mut text = String::from("schema = 1\n");
    for (n, plat, v) in rows {
        text.push_str(&format!(
            "\n[[pkg]]\nname = \"{n}\"\nplatform = \"{plat}\"\nprovider = \"script\"\nversion = \"{v}\"\nlocked_at = \"2026-11-25T20:11:03Z\"\n"
        ));
    }
    write(&b.root.join("dots.lock"), &text);
}

fn journal(b: &Sb) -> Vec<Value> {
    std::fs::read_to_string(b.home.join(".local/state/dots/journal.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn error_codes(d: &Value) -> Vec<String> {
    d["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect()
}

// ------------------------------------------------------------------ format and keying

#[test]
fn apply_locks_what_it_installed_and_relocking_is_byte_identical() {
    let b = sandbox(&[script("fd", "")]);
    set(&b, "fd", None, "1.0.0");
    let o = run(&b, &["apply", "--yes"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let text = lock_text(&b);
    let key = machine_key(&b);
    assert!(
        text.starts_with("# Generated by dots. Do not edit."),
        "{text}"
    );
    assert!(text.contains("schema = 1"), "{text}");
    assert!(text.contains("name = \"fd\""), "{text}");
    assert!(text.contains("provider = \"script\""), "{text}");
    assert!(text.contains("version = \"1.0.0\""), "{text}");
    // script versions depend on the platform: the row is keyed by this machine.
    assert!(text.contains(&format!("platform = \"{key}\"")), "{text}");

    // Nothing changed, so nothing is rewritten.
    let again = run(&b, &["apply", "--yes"]);
    assert_eq!(again.code, 0);
    assert_eq!(lock_text(&b), text);
    let plan = run(&b, &["plan", "--check"]);
    assert_eq!(
        plan.code, 0,
        "an up-to-date machine plans nothing: {}",
        plan.stdout
    );
}

#[test]
fn status_shows_the_locked_version() {
    let b = sandbox(&[script("fd", "")]);
    set(&b, "fd", None, "1.0.0");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let o = run(&b, &["status", "fd", "--json"]);
    let d = json(&o);
    validate("status", &d);
    assert_eq!(d["data"]["versions"]["locked"], "1.0.0");
    assert_eq!(d["data"]["versions"]["installed"], "1.0.0");
    let text = run(&b, &["status", "fd"]);
    assert!(text.stdout.contains("locked"), "{}", text.stdout);
}

#[test]
fn the_locked_version_is_kept_under_latest_but_a_new_machine_gets_it() {
    // The lock says 1.0.0; the provider offers 2.0.0. A machine that has nothing installed
    // still asks for the locked version (script providers cannot, so it reports drift).
    let b = sandbox(&[script("fd", "")]);
    let key = machine_key(&b);
    seed_lock(&b, &[("fd", &key, "1.0.0")]);
    set(&b, "fd", None, "2.0.0");
    let plan = json(&run(&b, &["plan", "--json"]));
    validate("plan", &plan);
    assert_eq!(plan["data"]["steps"][0]["action"], "install");
    assert_eq!(
        plan["data"]["lock"][0]["drift"],
        "locked 1.0.0, provider offers 2.0.0"
    );
    // The row of another platform is not this machine's row.
    let b2 = sandbox(&[script("fd", "")]);
    seed_lock(&b2, &[("fd", "other-1/x86_64", "1.0.0")]);
    set(&b2, "fd", None, "2.0.0");
    let plan = json(&run(&b2, &["plan", "--json"]));
    assert!(plan["data"]["lock"][0]["from"].is_null(), "{plan}");
    assert!(plan["data"]["lock"][0]["drift"].is_null(), "{plan}");
}

// ------------------------------------------------------------------ lock versus spec

#[test]
fn a_tightened_spec_shows_the_lock_row_moving_to_the_newest_match() {
    let b = sandbox(&[script("bottom", "version = \">=0.10\"")]);
    let key = machine_key(&b);
    seed_lock(&b, &[("bottom", &key, "0.9.6")]);
    set(&b, "bottom", Some("0.9.6"), "0.10.2");
    let plan = run(&b, &["plan", "--json"]);
    let d = json(&plan);
    validate("plan", &d);
    let row = &d["data"]["lock"][0];
    assert_eq!(row["package"], "bottom");
    assert_eq!(row["from"], "0.9.6");
    assert_eq!(row["to"], "0.10.2");
    assert_eq!(d["data"]["steps"][0]["action"], "upgrade");
    let text = run(&b, &["plan"]);
    assert!(
        text.stdout.contains("0.9.6") && text.stdout.contains("0.10.2"),
        "{}",
        text.stdout
    );
    assert!(text.stdout.contains("Lock"), "{}", text.stdout);
    // Applying rewrites exactly that row.
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(ver(&b, "bottom"), "0.10.2");
    assert!(
        lock_text(&b).contains("version = \"0.10.2\""),
        "{}",
        lock_text(&b)
    );
    assert!(!lock_text(&b).contains("0.9.6"));
}

#[test]
fn a_limited_provider_installs_what_it_offers_and_status_reports_drift() {
    let b = sandbox(&[script("less", "")]);
    let key = machine_key(&b);
    seed_lock(&b, &[("less", &key, "8.7.0")]);
    set(&b, "less", None, "8.7.1");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(ver(&b, "less"), "8.7.1");
    // The lock still honestly says 8.7.0.
    assert!(
        lock_text(&b).contains("version = \"8.7.0\""),
        "{}",
        lock_text(&b)
    );
    let s = run(&b, &["status", "--json"]);
    let d = json(&s);
    let item = d["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["package"] == "less")
        .unwrap();
    assert_eq!(item["state"], "drift", "{d}");
    assert!(item["detail"].as_str().unwrap().contains("locked 8.7.0"));
    // `--strict` treats drift as an error.
    assert_eq!(s.code, 2, "drift is a warning by default");
    assert_eq!(run(&b, &["status", "--strict"]).code, 1);
}

#[test]
fn a_version_installed_outside_dots_is_adopted_into_the_lock() {
    let b = sandbox(&[script("fd", "version = \"1.2\"")]);
    set(&b, "fd", Some("1.2.9"), "1.3.0");
    let plan = json(&run(&b, &["plan", "--json"]));
    assert!(plan["data"]["steps"].as_array().unwrap().is_empty());
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(ver(&b, "fd"), "1.2.9", "not reinstalled");
    assert!(lock_text(&b).contains("version = \"1.2.9\""));
    assert!(
        journal(&b)
            .iter()
            .any(|e| e["op"] == "adopt" && e["pkg"] == "fd")
    );
}

#[test]
fn an_unattended_downgrade_exits_1_and_says_how_to_allow_it() {
    let b = sandbox(&[script("fd", "version = \"1.0.0\"")]);
    set(&b, "fd", Some("2.0.0"), "1.0.0");
    let plan = run(&b, &["plan"]);
    assert!(plan.stdout.contains("downgrade"), "{}", plan.stdout);
    assert!(plan.stdout.contains('\u{26a0}'), "{}", plan.stdout);
    let o = run(&b, &["apply", "--yes", "--no-input"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(
        format!("{}{}", o.stdout, o.stderr).contains("--allow-downgrade"),
        "{}{}",
        o.stdout,
        o.stderr
    );
    assert_eq!(ver(&b, "fd"), "2.0.0");
    let o = run(&b, &["apply", "--yes", "--no-input", "--allow-downgrade"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(journal(&b).iter().any(|e| e["op"] == "downgrade"));
}

// ------------------------------------------------------------------ upgrade

#[test]
fn upgrade_stays_inside_a_prefix_and_rewrites_the_lock_and_journal() {
    let b = sandbox(&[script("fd", "version = \"0.11\"")]);
    let key = machine_key(&b);
    seed_lock(&b, &[("fd", &key, "0.11.2")]);
    // 0.12.0 exists, but it is outside the spec; script cannot look inside the prefix.
    set(&b, "fd", Some("0.11.2"), "0.12.0");
    let o = run(&b, &["upgrade", "fd", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    validate("upgrade", &d);
    assert!(
        d["data"]["upgrade"]["targets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        d["data"]["upgrade"]["skipped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("can't find newest 0.11.x via script"),
        "{d}"
    );
    assert_eq!(ver(&b, "fd"), "0.11.2");
    // A newer 0.11.x does move, journaled, with only this platform's row rewritten.
    set(&b, "fd", Some("0.11.2"), "0.11.9");
    let o = run(&b, &["upgrade", "fd", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    validate("upgrade", &d);
    assert_eq!(d["data"]["upgrade"]["targets"][0]["from"], "0.11.2");
    assert_eq!(d["data"]["upgrade"]["targets"][0]["to"], "0.11.9");
    assert_eq!(ver(&b, "fd"), "0.11.9");
    assert!(lock_text(&b).contains("version = \"0.11.9\""));
    assert!(
        journal(&b)
            .iter()
            .any(|e| e["op"] == "upgrade" && e["pkg"] == "fd" && e["to"] == "0.11.9")
    );
}

/// `fd` (a sudo step: `requires_sudo`) and `bat` (ordinary), both installed and behind.
fn sudo_upgrade_sandbox() -> Sb {
    let b = sandbox(&[script("fd", "requires_sudo = true"), script("bat", "")]);
    let key = machine_key(&b);
    seed_lock(&b, &[("fd", &key, "1.0.0"), ("bat", &key, "1.0.0")]);
    set(&b, "fd", Some("1.0.0"), "1.1.0");
    set(&b, "bat", Some("1.0.0"), "1.1.0");
    b
}

#[test]
fn plain_upgrade_skips_sudo_steps_warns_and_exits_zero() {
    let b = sudo_upgrade_sandbox();
    let o = run(&b, &["upgrade", "fd", "bat"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("1 step needs sudo and was not applied")
            && o.stdout.contains("dots upgrade --sudo-only")
            && o.stdout.contains("dots upgrade --with-sudo"),
        "{}",
        o.stdout
    );
    assert_eq!(ver(&b, "bat"), "1.1.0");
    assert_eq!(ver(&b, "fd"), "1.0.0");
    // Nothing skipped is recorded.
    let lock = lock_text(&b);
    let row = |n: &str| {
        lock.split("[[pkg]]")
            .find(|r| r.contains(&format!("name = \"{n}\"")))
            .unwrap_or_default()
            .to_string()
    };
    assert!(row("fd").contains("version = \"1.0.0\""), "{lock}");
    assert!(row("bat").contains("version = \"1.1.0\""), "{lock}");
    assert!(
        journal(&b)
            .iter()
            .all(|e| !(e["op"] == "upgrade" && e["pkg"] == "fd"))
    );
    let o = run(&b, &["upgrade", "fd", "bat", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    validate("upgrade", &d);
    assert_eq!(d["data"]["sudo"]["mode"], "skip");
    assert_eq!(d["data"]["sudo"]["skipped"][0]["name"], "fd");
}

#[test]
fn upgrade_sudo_only_runs_just_the_sudo_steps() {
    let b = sudo_upgrade_sandbox();
    let o = run(&b, &["upgrade", "fd", "bat", "--sudo-only", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    validate("upgrade", &d);
    assert_eq!(d["data"]["sudo"]["mode"], "only");
    assert_eq!(ver(&b, "fd"), "1.1.0");
    assert_eq!(ver(&b, "bat"), "1.0.0");
}

#[test]
fn upgrade_with_sudo_runs_everything() {
    let b = sudo_upgrade_sandbox();
    let o = run(&b, &["upgrade", "fd", "bat", "--with-sudo", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    validate("upgrade", &d);
    assert_eq!(d["data"]["sudo"]["mode"], "with");
    assert!(d["data"]["sudo"]["skipped"].as_array().unwrap().is_empty());
    assert_eq!(ver(&b, "fd"), "1.1.0");
    assert_eq!(ver(&b, "bat"), "1.1.0");
}

#[test]
fn upgrade_sudo_flags_conflict() {
    let b = sudo_upgrade_sandbox();
    let o = run(&b, &["upgrade", "fd", "--with-sudo", "--sudo-only"]);
    assert_eq!(o.code, 64, "{}{}", o.stdout, o.stderr);
    assert!(o.stderr.contains("cannot be used with"), "{}", o.stderr);
    assert_eq!(ver(&b, "fd"), "1.0.0");
}

#[test]
fn upgrade_rewrites_only_this_platforms_rows() {
    let b = sandbox(&[script("fd", "")]);
    let key = machine_key(&b);
    seed_lock(
        &b,
        &[("fd", &key, "1.0.0"), ("fd", "other-1/x86_64", "1.0.0")],
    );
    set(&b, "fd", Some("1.0.0"), "1.1.0");
    assert_eq!(run(&b, &["upgrade", "--all"]).code, 0);
    let text = lock_text(&b);
    assert_eq!(text.matches("version = \"1.1.0\"").count(), 1, "{text}");
    assert_eq!(text.matches("version = \"1.0.0\"").count(), 1, "{text}");
    assert!(text.contains("platform = \"other-1/x86_64\""));
}

#[test]
fn a_dependent_blocks_the_upgrade_and_is_named() {
    let b = sandbox(&[
        script("rust", ""),
        script("kv", "requires = [{ name = \"rust\", version = \"1.75\" }]"),
    ]);
    set(&b, "rust", Some("1.75.3"), "1.80.0");
    set(&b, "kv", Some("1.0.0"), "1.0.0");
    let o = run(&b, &["upgrade", "rust", "--json"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    assert_eq!(error_codes(&d), ["upgrade-blocked"]);
    let msg = d["errors"][0]["message"].as_str().unwrap();
    assert!(msg.contains("`kv`") && msg.contains("1.75"), "{msg}");
    assert_eq!(ver(&b, "rust"), "1.75.3");
    // With --all it is only a note.
    let o = run(&b, &["upgrade", "--all", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
}

#[test]
fn upgrade_needs_packages_or_all() {
    let b = sandbox(&[script("fd", "")]);
    let o = run(&b, &["upgrade"]);
    assert_eq!(o.code, 64, "{}{}", o.stdout, o.stderr);
}

#[test]
fn upgrade_never_commits_unless_asked() {
    let b = sandbox(&[script("fd", "")]);
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&b.root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    set(&b, "fd", None, "1.0.0");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "lock"]);
    let before = git(&["rev-parse", "HEAD"]);
    set(&b, "fd", Some("1.0.0"), "1.1.0");
    assert_eq!(run(&b, &["upgrade", "fd"]).code, 0);
    assert_eq!(
        git(&["rev-parse", "HEAD"]),
        before,
        "no commit without --commit"
    );
    assert!(git(&["status", "--porcelain"]).contains("dots.lock"));

    set(&b, "fd", Some("1.1.0"), "1.2.0");
    let env = [
        ("GIT_AUTHOR_NAME", "t"),
        ("GIT_AUTHOR_EMAIL", "t@example.com"),
        ("GIT_COMMITTER_NAME", "t"),
        ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ];
    let o = run_env(&b, &env, &["upgrade", "fd", "--commit"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let msg = git(&["log", "-1", "--format=%B"]);
    assert!(
        msg.starts_with("dots: upgrade fd 1.1.0\u{2192}1.2.0"),
        "{msg}"
    );
    assert!(
        msg.contains("Dots-Op: upgrade") && msg.contains("Dots-Package: fd"),
        "{msg}"
    );
    assert!(git(&["status", "--porcelain"]).trim().is_empty());
}

// ------------------------------------------------------------------ pin and unpin

#[test]
fn pin_writes_the_version_and_unpin_removes_it_keeping_the_formatting() {
    let b = sandbox(&[script("bottom", "# the pager\ndescription = \"x\"")]);
    set(&b, "bottom", Some("0.10.2"), "0.10.2");
    let file = b.root.join("packages/bottom/package.toml");
    let original = std::fs::read_to_string(&file).unwrap();
    let o = run(&b, &["pin", "bottom", "0.10.2", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    validate("pin", &json(&o));
    let pinned = std::fs::read_to_string(&file).unwrap();
    assert!(pinned.contains("version = \"0.10.2\""), "{pinned}");
    assert!(pinned.contains("# the pager"), "comments survive: {pinned}");
    assert!(journal(&b).iter().any(|e| e["op"] == "pin"));
    // script has no native pin; the report says nothing ran.
    assert!(json(&o)["data"]["native"].is_null());

    let o = run(&b, &["unpin", "bottom", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    validate("unpin", &json(&o));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    assert!(journal(&b).iter().any(|e| e["op"] == "unpin"));
}

#[test]
fn pin_over_a_range_is_undone_by_unpin_and_unpin_leaves_user_ranges() {
    let b = sandbox(&[script("bottom", "version = \">=0.10\"")]);
    set(&b, "bottom", Some("0.10.2"), "0.10.2");
    let file = b.root.join("packages/bottom/package.toml");
    let original = std::fs::read_to_string(&file).unwrap();
    assert_eq!(run(&b, &["pin", "bottom", "0.10.2"]).code, 0);
    let pinned = std::fs::read_to_string(&file).unwrap();
    assert!(pinned.contains("version = \"0.10.2\""), "{pinned}");
    assert!(pinned.contains("dots:pinned-from = \">=0.10\""), "{pinned}");
    assert_eq!(run(&b, &["unpin", "bottom"]).code, 0);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    // A range the user wrote is not a pin: unpin does not delete it.
    let o = run(&b, &["unpin", "bottom", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    assert_eq!(json(&o)["data"]["changed"], false);
}

#[test]
fn pin_refuses_when_an_override_sets_the_version() {
    let b = sandbox(&[script("bottom", "")]);
    set(&b, "bottom", Some("0.10.2"), "0.10.2");
    write(
        &b.root.join("overrides/packages/bottom/override.toml"),
        "schema = 1\nversion = \"0.9\"\n",
    );
    let file = b.root.join("packages/bottom/package.toml");
    let original = std::fs::read_to_string(&file).unwrap();
    let o = run(&b, &["pin", "bottom", "0.10.2", "--json"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    assert_eq!(error_codes(&d), ["pin-unsupported"]);
    let msg = d["errors"][0]["message"].as_str().unwrap();
    assert!(
        msg.contains("overrides/packages/bottom/override.toml"),
        "{msg}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
}

#[test]
fn pin_checks_dependents_constraints_and_writes_nothing_on_conflict() {
    let b = sandbox(&[
        script("rust", ""),
        script(
            "kv",
            "requires = [{ name = \"rust\", version = \">=1.80\" }]",
        ),
    ]);
    set(&b, "rust", Some("1.85.0"), "1.85.0");
    set(&b, "kv", Some("1.0.0"), "1.0.0");
    let file = b.root.join("packages/rust/package.toml");
    let original = std::fs::read_to_string(&file).unwrap();
    let o = run(&b, &["pin", "rust", "1.75.0", "--json"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert_eq!(error_codes(&json(&o)), ["version-conflict"]);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    assert!(!journal(&b).iter().any(|e| e["op"] == "pin"));
    // A version inside the dependent's range is fine.
    assert_eq!(run(&b, &["pin", "rust", "1.85.0"]).code, 0);
}

#[test]
fn pin_without_a_version_holds_what_is_installed() {
    let b = sandbox(&[script("bottom", "")]);
    set(&b, "bottom", Some("0.10.2"), "0.11.0");
    assert_eq!(run(&b, &["pin", "bottom"]).code, 0);
    let text = std::fs::read_to_string(b.root.join("packages/bottom/package.toml")).unwrap();
    assert!(text.contains("version = \"0.10.2\""), "{text}");
    // Not installed and no version: nothing to hold.
    let b = sandbox(&[script("bottom", "")]);
    set(&b, "bottom", None, "0.11.0");
    let o = run(&b, &["pin", "bottom", "--json"]);
    assert_eq!(o.code, 1);
    assert_eq!(error_codes(&json(&o)), ["pin-unsupported"]);
}

#[test]
fn pin_rejects_a_version_outside_the_grammar_and_unknown_packages() {
    let b = sandbox(&[script("bottom", "")]);
    let o = run(&b, &["pin", "bottom", "~1.2", "--json"]);
    assert_eq!(o.code, 1);
    assert_eq!(error_codes(&json(&o)), ["version-spec-invalid"]);
    let o = run(&b, &["pin", "nope", "1.2.3", "--json"]);
    assert_eq!(error_codes(&json(&o)), ["unknown-package"]);
}

#[test]
fn pin_commit_makes_one_commit_with_trailers() {
    let b = sandbox(&[script("bottom", "")]);
    let env = [
        ("GIT_AUTHOR_NAME", "t"),
        ("GIT_AUTHOR_EMAIL", "t@example.com"),
        ("GIT_COMMITTER_NAME", "t"),
        ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ];
    for args in [
        &["init", "-q"][..],
        &["add", "-A"],
        &["commit", "-q", "-m", "init"],
    ] {
        let o = Command::new("git")
            .arg("-C")
            .arg(&b.root)
            .args(args)
            .envs(env)
            .output()
            .unwrap();
        assert!(o.status.success());
    }
    let o = run_env(&b, &env, &["pin", "bottom", "0.10.2", "--commit"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let log = Command::new("git")
        .arg("-C")
        .arg(&b.root)
        .args(["log", "-1", "--format=%B"])
        .output()
        .unwrap();
    let msg = String::from_utf8_lossy(&log.stdout).into_owned();
    assert!(msg.starts_with("dots: pin bottom 0.10.2"), "{msg}");
    assert!(
        msg.contains("Dots-Op: pin") && msg.contains("Dots-Package: bottom"),
        "{msg}"
    );
}

// ------------------------------------------------------------------ repair

const CONFLICTED: &str = "schema = 1

[[pkg]]
name = \"fd\"
platform = \"*\"
provider = \"cargo\"
version = \"1.0.0\"
locked_at = \"2026-11-25T20:11:03Z\"

<<<<<<< HEAD
[[pkg]]
name = \"bat\"
platform = \"*\"
provider = \"cargo\"
version = \"0.24.0\"
locked_at = \"2026-11-25T20:11:03Z\"
=======
[[pkg]]
name = \"rg\"
platform = \"*\"
provider = \"cargo\"
version = \"14.0.0\"
locked_at = \"2026-11-25T20:11:03Z\"
>>>>>>> feature
";

#[test]
fn a_conflicted_lock_blocks_every_command_but_lock_fix() {
    let b = sandbox(&[script("fd", "")]);
    set(&b, "fd", None, "1.0.0");
    write(&b.root.join("dots.lock"), CONFLICTED);
    for args in [
        &["status", "--json"][..],
        &["plan", "--json"],
        &["apply", "--yes", "--json"],
        &["lint", "--json"],
        &["upgrade", "--all", "--json"],
    ] {
        let o = run(&b, args);
        assert_eq!(o.code, 1, "{args:?}: {}{}", o.stdout, o.stderr);
        assert_eq!(error_codes(&json(&o)), ["lock-conflicted"], "{args:?}");
    }
    let o = run(&b, &["lock", "--fix", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    validate("lock", &d);
    assert_eq!(d["data"]["status"], "repaired");
    assert_eq!(d["data"]["had_markers"], true);
    // fd, bat and rg: every row that is valid. (The two with no package in the repo are stale
    // and dropped as well, since the repo only has `fd`.)
    let text = lock_text(&b);
    assert!(!text.contains("<<<<<<<") && !text.contains(">>>>>>>"));
    assert!(text.contains("name = \"fd\""), "{text}");
    // Every command works again (fd is merely not installed: status says so, no error).
    assert!(
        json(&run(&b, &["status", "--json"]))["errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        run(&b, &["lock", "--fix"]).code,
        0,
        "a valid lock needs no repair"
    );
}

#[test]
fn lock_fix_keeps_every_valid_row_of_a_broken_file() {
    let b = sandbox(&[script("fd", ""), script("bat", ""), script("rg", "")]);
    write(&b.root.join("dots.lock"), CONFLICTED);
    let o = run(&b, &["lock", "--fix", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let text = lock_text(&b);
    for n in ["fd", "bat", "rg"] {
        assert!(
            text.contains(&format!("name = \"{n}\"")),
            "{n} kept: {text}"
        );
    }
    assert_eq!(json(&o)["data"]["kept"], 3);
}

#[test]
fn an_unparseable_lock_is_lock_invalid_and_repairable() {
    let b = sandbox(&[script("fd", "")]);
    write(&b.root.join("dots.lock"), "schema = 1\n[[pkg]\nname = ");
    let o = run(&b, &["status", "--json"]);
    assert_eq!(error_codes(&json(&o)), ["lock-invalid"]);
    assert_eq!(run(&b, &["lock", "--fix"]).code, 0);
    assert!(
        json(&run(&b, &["status", "--json"]))["errors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

// ------------------------------------------------------------------ doctor and fetch hashes

#[test]
fn doctor_flags_stale_lock_rows() {
    let b = sandbox(&[script("fd", "version = \">=2\"")]);
    let key = machine_key(&b);
    set(&b, "fd", Some("2.0.0"), "2.0.0");
    seed_lock(&b, &[("fd", &key, "1.0.0"), ("ghost", &key, "3.0.0")]);
    let o = run(&b, &["doctor", "--json"]);
    let d = json(&o);
    let checks = d["data"]["checks"].as_array().unwrap();
    let lock = checks.iter().find(|c| c["id"] == "lock").unwrap();
    let msgs: Vec<&str> = lock["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["message"].as_str().unwrap())
        .collect();
    assert!(msgs.iter().any(|m| m.contains("ghost")), "{msgs:?}");
    assert!(
        msgs.iter().any(|m| m.contains("stale lock row")),
        "{msgs:?}"
    );
    // `lock --fix` drops the row of the package that is gone, and only that.
    assert_eq!(run(&b, &["lock", "--fix"]).code, 0);
    let text = lock_text(&b);
    assert!(
        !text.contains("ghost") && text.contains("name = \"fd\""),
        "{text}"
    );
}

/// Serve `file` over http on localhost, reading it afresh for every request.
fn serve(file: PathBuf) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = std::fs::read(&file).unwrap_or_default();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}/install.sh")
}

#[test]
fn a_scripts_fetch_hash_is_recorded_on_first_use_and_checked_afterwards() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("install.sh");
    write(&file, "touch \"$HOME/tool.ver\"\n");
    let url = serve(file.clone());
    let b = sandbox(&[(
        "tool".to_string(),
        format!(
            "[providers.script]\ncheck = \"test -f $HOME/tool.ver\"\nversionless = true\ninstall = \"sh {{fetched}}\"\nfetch = {{ url = \"{url}\" }}\n"
        ),
    )]);
    let o = run(&b, &["apply", "--yes"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(b.home.join("tool.ver").exists(), "{}{}", o.stdout, o.stderr);
    let text = lock_text(&b);
    assert!(text.contains("[[content]]"), "{text}");
    assert!(text.contains("item = \"fetch:tool\""), "{text}");
    assert!(text.contains("sha256 = \""), "{text}");
    // The file changes upstream: a fresh install is refused against the locked hash.
    std::fs::remove_file(b.home.join("tool.ver")).unwrap();
    write(&file, "touch \"$HOME/tool.ver\"\necho tampered\n");
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("checksum mismatch"), "{}", o.stdout);
    assert!(!b.home.join("tool.ver").exists());
}

// ------------------------------------------------------------------ versions through the CLI

#[test]
fn a_version_conflict_names_both_sides_with_their_files() {
    let b = sandbox(&[
        script("rust", ""),
        script(
            "kv",
            "requires = [{ name = \"rust\", version = \">=1.80\" }]",
        ),
    ]);
    write(
        &b.root.join("overrides/packages/rust/override.toml"),
        "schema = 1\nversion = \"1.75\"\n",
    );
    let o = run(&b, &["plan", "--json"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    let d = json(&o);
    assert_eq!(error_codes(&d), ["version-conflict"]);
    let files: Vec<&str> = d["errors"][0]["provenance"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["file"].as_str())
        .collect();
    assert!(files.contains(&"packages/kv/package.toml"), "{files:?}");
    assert!(
        files.contains(&"overrides/packages/rust/override.toml"),
        "{files:?}"
    );
    let msg = d["errors"][0]["message"].as_str().unwrap();
    assert!(
        msg.contains("machine → all → kv → requires rust >=1.80"),
        "{msg}"
    );
    assert!(msg.contains("repo override"), "{msg}");
}

#[test]
fn an_invalid_version_spec_fails_every_command_with_the_five_forms() {
    let b = sandbox(&[script("fd", "version = \"~1.2\"")]);
    let o = run(&b, &["status", "--json"]);
    assert_eq!(o.code, 1);
    let d = json(&o);
    assert_eq!(error_codes(&d), ["version-spec-invalid"]);
    let hint = d["errors"][0]["hint"].as_str().unwrap();
    for f in ["latest", "1.2.3", "1.2", ">=1.2", "git:<ref>"] {
        assert!(hint.contains(f), "{hint}");
    }
    assert_eq!(d["errors"][0]["provenance"][0]["line"], 3);
}
