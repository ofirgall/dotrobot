//! Task 5.11: `dots migrate` and orphan/unmanaged classification, through the real binary in a
//! `--home` sandbox. Two providers are real: `script` (the package's own commands) and `cargo`
//! (driven by a fake `cargo` shell script that keeps `.crates2.json` like the real one), plus a
//! fake `brew` for the dependency scenario.

mod common;

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

fn make_exe(p: &Path, text: &str) {
    write(p, text);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A cargo that installs and removes one crate by writing what the real one writes. The file
/// `$HOME/cargo-install-fails` makes `install` fail; `$HOME/cargo-no-record` makes it succeed
/// without recording anything (a broken install).
const FAKE_CARGO: &str = r#"#!/bin/sh
case "$1" in
  install)
    [ -f "$HOME/cargo-install-fails" ] && { echo "boom" >&2; exit 1; }
    for a in "$@"; do case "$a" in install|--*) ;; *) name="${a%%@*}" ;; esac; done
    [ -f "$HOME/cargo-no-record" ] && exit 0
    mkdir -p "$CARGO_HOME/bin"
    printf '#!/bin/sh\n' > "$CARGO_HOME/bin/$name"
    chmod +x "$CARGO_HOME/bin/$name"
    printf '{"installs":{"%s 0.30.0 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["%s"]}}}' "$name" "$name" > "$CARGO_HOME/.crates2.json"
    ;;
  uninstall)
    rm -f "$CARGO_HOME/bin/$2"
    printf '{"installs":{}}' > "$CARGO_HOME/.crates2.json"
    ;;
  *) exit 2 ;;
esac
"#;

/// `bat`: a script provider (installed when `$HOME/bat.installed` exists) and cargo.
fn pkg_body(prefer: &str, script_uninstall: &str) -> String {
    format!(
        "schema = 1\nname = \"bat\"\n{prefer}\n[providers.script]\ncheck = \"test -f $HOME/bat.installed\"\nversion = \"echo 1.0.0\"\ninstall = \"touch $HOME/bat.installed\"\nuninstall = \"{script_uninstall}\"\n\n[providers.cargo]\n"
    )
}

fn sandbox(prefer: &str, script_uninstall: &str) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    write(
        &root.join("packages/bat/package.toml"),
        &pkg_body(prefer, script_uninstall),
    );
    write(
        &root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"bat\"]\n",
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    make_exe(&home.join(".cargo/bin/cargo"), FAKE_CARGO);
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n",
            root.display().to_string()
        ),
    );
    let sb = Sb {
        tmp,
        home,
        root,
        machine,
    };
    // `bat` is installed by the script provider, through dots.
    let o = run(&sb, &["apply", "--yes", "--packages-only"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(sb.home.join("bat.installed").exists());
    sb
}

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_env(b: &Sb, env: &[(&str, &str)], args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "script,cargo,brew")
        // Provider tools keep the real HOME, not `--home`: these tests' "real" HOME is the
        // sandbox, which holds their fake cargo.
        .env("HOME", &b.home)
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

fn run(b: &Sb, args: &[&str]) -> Out {
    run_env(b, &[], args)
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", o.stdout, o.stderr))
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

fn package_toml(b: &Sb) -> String {
    std::fs::read_to_string(b.root.join("packages/bat/package.toml")).unwrap()
}

fn lock(b: &Sb) -> String {
    std::fs::read_to_string(b.root.join("dots.lock")).unwrap_or_default()
}

fn journal(b: &Sb) -> Vec<Value> {
    std::fs::read_to_string(b.home.join(".local/state/dots/journal.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn state(b: &Sb) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(b.home.join(".local/state/dots/state.json")).unwrap(),
    )
    .unwrap()
}

const PREFER_SCRIPT: &str = "prefer = [\"script\", \"cargo\"]";

#[test]
fn migrate_installs_verifies_removes_and_records_everything() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    assert!(lock(&b).contains("provider = \"script\""), "{}", lock(&b));
    let o = run(&b, &["migrate", "bat", "--to", "cargo", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let doc = json(&o);
    validate("migrate", &doc);
    let d = &doc["data"];
    assert_eq!(d["from"], serde_json::json!(["script"]));
    assert_eq!(d["to"], "cargo");
    assert_eq!(d["installed"], true);
    assert_eq!(d["removed"], serde_json::json!(["script"]));
    assert_eq!(d["version"], "0.30.0");

    // Installed by cargo, gone from the old provider.
    assert!(b.home.join(".cargo/bin/bat").exists());
    assert!(!b.home.join("bat.installed").exists());
    // The declaration follows.
    let toml = package_toml(&b);
    assert!(toml.contains("prefer = [\"cargo\", \"script\"]"), "{toml}");
    // dots.lock: the script row is gone, cargo's platform-independent row is there.
    let lock = lock(&b);
    assert!(!lock.contains("provider = \"script\""), "{lock}");
    assert!(
        lock.contains("provider = \"cargo\"") && lock.contains("platform = \"*\""),
        "{lock}"
    );
    assert!(lock.contains("version = \"0.30.0\""), "{lock}");
    // Ownership and journal.
    let st = state(&b);
    assert_eq!(st["packages"]["bat"]["provider"], "cargo");
    let ops: Vec<(String, Option<String>)> = journal(&b)
        .iter()
        .filter(|e| e["run"] != journal(&b)[0]["run"])
        .map(|e| {
            (
                e["op"].as_str().unwrap().to_string(),
                e["provider"].as_str().map(str::to_string),
            )
        })
        .collect();
    assert_eq!(
        ops,
        [
            ("install".into(), Some("cargo".into())),
            ("remove".into(), Some("script".into())),
            ("migrate_provider".into(), Some("cargo".into())),
        ]
    );
    // The new choice is what status resolves to afterwards, with nothing orphaned.
    let s = run(&b, &["status", "--all", "--json"]);
    let items = json(&s)["data"]["items"].clone();
    let bat = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "bat")
        .unwrap();
    assert_eq!(bat["provider"], "cargo");
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["state"] != "orphan"),
        "{items}"
    );
}

#[test]
fn a_ranking_that_already_prefers_the_target_leaves_the_package_file_alone() {
    // No `prefer`: cargo sorts before script, so only the sticky install kept script.
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    // Dropping `prefer` leaves only the sticky install holding script in place.
    write(
        &b.root.join("packages/bat/package.toml"),
        &pkg_body("", "rm -f $HOME/bat.installed"),
    );
    let before = package_toml(&b);
    let o = run(&b, &["migrate", "bat", "--to", "cargo", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let d = json(&o)["data"].clone();
    assert_eq!(d["changed"], false);
    assert_eq!(package_toml(&b), before);
    assert!(b.home.join(".cargo/bin/bat").exists());
}

#[test]
fn a_failed_install_leaves_the_old_install_working_and_nothing_recorded() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    write(&b.home.join("cargo-install-fails"), "");
    let (toml, lock_before) = (package_toml(&b), lock(&b));
    let o = run(&b, &["migrate", "bat", "--to", "cargo"]);
    assert_ne!(o.code, 0);
    assert!(
        o.stdout.contains("old install is untouched") || o.stderr.contains("untouched"),
        "{}{}",
        o.stdout,
        o.stderr
    );
    assert!(b.home.join("bat.installed").exists());
    assert_eq!(package_toml(&b), toml);
    assert_eq!(lock(&b), lock_before);
    assert_eq!(state(&b)["packages"]["bat"]["provider"], "script");
}

#[test]
fn an_install_that_does_not_verify_is_taken_back_and_the_old_one_stays() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    write(&b.home.join("cargo-no-record"), "");
    let (toml, lock_before) = (package_toml(&b), lock(&b));
    let o = run(&b, &["migrate", "bat", "--to", "cargo", "--json"]);
    assert_ne!(o.code, 0);
    assert_eq!(json(&o)["errors"][0]["code"], "check-failed-after-install");
    assert!(b.home.join("bat.installed").exists());
    assert_eq!(package_toml(&b), toml);
    assert_eq!(lock(&b), lock_before);
}

#[test]
fn a_failed_removal_keeps_both_installs_and_records_nothing() {
    let b = sandbox(PREFER_SCRIPT, "false");
    let (toml, lock_before) = (package_toml(&b), lock(&b));
    let o = run(&b, &["migrate", "bat", "--to", "cargo", "--json"]);
    assert_ne!(o.code, 0);
    let doc = json(&o);
    assert_eq!(doc["errors"][0]["code"], "action-failed");
    assert!(b.home.join("bat.installed").exists());
    assert!(b.home.join(".cargo/bin/bat").exists());
    assert_eq!(package_toml(&b), toml);
    assert_eq!(lock(&b), lock_before);
    assert_eq!(state(&b)["packages"]["bat"]["provider"], "script");
}

#[test]
fn migrate_refuses_what_it_cannot_do() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    let code = |args: &[&str]| {
        let o = run(&b, &[&["--json"][..], args].concat());
        let doc = json(&o);
        (
            o.code,
            doc["errors"][0]["code"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(code(&["migrate", "bat", "--to", "pip"]).1, "plan-failed");
    assert_eq!(
        code(&["migrate", "nope", "--to", "cargo"]).1,
        "unknown-package"
    );
    // A provider that is a candidate but not available on this machine.
    let o = run_env(
        &b,
        &[("DOTS_PROVIDERS", "script")],
        &["--json", "migrate", "bat", "--to", "cargo"],
    );
    assert_ne!(o.code, 0);
    assert!(b.home.join("bat.installed").exists());
    // Nothing installed anywhere.
    std::fs::remove_file(b.home.join("bat.installed")).unwrap();
    assert_eq!(code(&["migrate", "bat", "--to", "cargo"]).1, "plan-failed");
}

#[test]
fn migrate_without_to_is_a_usage_error() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    let o = run(&b, &["migrate", "bat"]);
    assert_eq!(o.code, 64, "{}{}", o.stdout, o.stderr);
}

// ---- orphans and unmanaged ----------------------------------------------------------------

/// A fake brew whose `info --json=v2 --installed` lists `jq` (asked for) and `oniguruma`
/// (pulled in as a dependency of jq), and `stray` (asked for by hand).
const FAKE_BREW: &str = r#"#!/bin/sh
cat <<'EOF'
{"formulae":[
 {"name":"jq","tap":"homebrew/core","installed":[{"version":"1.7.1","installed_on_request":true}],"linked_keg":"1.7.1","pinned":false},
 {"name":"oniguruma","tap":"homebrew/core","installed":[{"version":"6.9.9","installed_on_request":false}],"linked_keg":"6.9.9","pinned":false},
 {"name":"stray","tap":"homebrew/core","installed":[{"version":"1.0","installed_on_request":true}],"linked_keg":"1.0","pinned":false},
 {"name":"gone","tap":"homebrew/core","installed":[{"version":"2.0","installed_on_request":true}],"linked_keg":"2.0","pinned":false}
],"casks":[]}
EOF
"#;

fn brew_sandbox() -> (Sb, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    write(
        &root.join("packages/jq/package.toml"),
        "schema = 1\nname = \"jq\"\n[providers.brew]\n",
    );
    write(
        &root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"jq\"]\n",
    );
    let home = tmp.path().join("home");
    let prefix = tmp.path().join("brew");
    make_exe(&prefix.join("bin/brew"), FAKE_BREW);
    // dots installed jq, and `gone` (no longer declared).
    write(
        &home.join(".local/state/dots/state.json"),
        &serde_json::json!({
            "schema": 1,
            "packages": {
                "jq": { "provider": "brew", "name": "jq", "version": "1.7.1", "run": "r-1", "ts": "2026-01-01T00:00:00Z" },
                "gone": { "provider": "brew", "name": "gone", "version": "2.0", "run": "r-1", "ts": "2026-01-01T00:00:00Z" }
            }
        })
        .to_string(),
    );
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n",
            root.display().to_string()
        ),
    );
    let prefix = prefix.display().to_string();
    (
        Sb {
            tmp,
            home,
            root,
            machine,
        },
        prefix,
    )
}

fn items(o: &Out) -> Vec<Value> {
    json(o)["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{}{}", o.stdout, o.stderr))
        .clone()
}

#[test]
fn a_brew_dependency_is_not_an_orphan() {
    let (b, prefix) = brew_sandbox();
    let env = [("HOMEBREW_PREFIX", prefix.as_str())];
    let o = run_env(&b, &env, &["status", "--all", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    validate("status", &json(&o));
    let items = items(&o);
    let find = |name: &str| items.iter().find(|i| i["name"] == name).unwrap();
    // oniguruma came with jq: not an orphan, and labelled as a dependency.
    assert_eq!(find("oniguruma")["state"], "unmanaged");
    assert!(
        find("oniguruma")["detail"]
            .as_str()
            .unwrap()
            .contains("dependency"),
        "{}",
        find("oniguruma")
    );
    // Installed by hand: unmanaged, not a dependency.
    assert_eq!(find("stray")["state"], "unmanaged");
    assert_eq!(find("stray")["detail"], "installed outside dots");
    // Installed by dots and no longer declared: the orphan.
    assert_eq!(find("gone")["state"], "orphan");
    let orphans: Vec<&str> = items
        .iter()
        .filter(|i| i["state"] == "orphan")
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(orphans, ["gone"]);

    // Without --all the unmanaged ones stay out of sight.
    let o = run_env(&b, &env, &["status", "--json"]);
    let names: Vec<String> = json(&o)["data"]["items"]
        .as_array()
        .map(|a| a.iter().map(|i| i["name"].to_string()).collect())
        .unwrap_or_default();
    assert!(
        !names
            .iter()
            .any(|n| n.contains("oniguruma") || n.contains("stray")),
        "{names:?}"
    );
}

#[test]
fn migrate_edits_the_override_that_sets_prefer() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    let ov = b.root.join("overrides/packages/bat/override.toml");
    write(&ov, "schema = 1\nprefer = [\"script\", \"cargo\"]\n");
    let before = package_toml(&b);
    let o = run(&b, &["migrate", "bat", "--to", "cargo", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let text = std::fs::read_to_string(&ov).unwrap();
    assert!(text.contains("prefer = [\"cargo\", \"script\"]"), "{text}");
    assert_eq!(
        package_toml(&b),
        before,
        "the shadowed definition was edited"
    );
    assert_eq!(
        json(&o)["warnings"].as_array().map_or(0, Vec::len),
        0,
        "{}",
        o.stdout
    );
    // The ranking now picks cargo: nothing is left to apply.
    let o = run(&b, &["plan", "--check"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
}

#[test]
fn migrate_edits_the_platform_table_that_sets_prefer() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let f = b.root.join("packages/bat/package.toml");
    let mut t = package_toml(&b);
    t.push_str(&format!(
        "\n[platform.{os}]\nprefer = [\"script\", \"cargo\"]\n"
    ));
    write(&f, &t);
    let o = run(&b, &["migrate", "bat", "--to", "cargo", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let text = package_toml(&b);
    assert!(
        text.contains("prefer = [\"script\", \"cargo\"]\n[providers")
            || text.starts_with("schema = 1\nname = \"bat\"\nprefer = [\"script\", \"cargo\"]"),
        "{text}"
    );
    assert!(
        text.split(&format!("[platform.{os}]"))
            .nth(1)
            .unwrap()
            .contains("prefer = [\"cargo\", \"script\"]"),
        "{text}"
    );
    assert_eq!(
        json(&o)["warnings"].as_array().map_or(0, Vec::len),
        0,
        "{}",
        o.stdout
    );
}

#[test]
fn bins_does_not_call_a_declared_packages_other_copy_an_orphan() {
    let b = sandbox(PREFER_SCRIPT, "rm -f $HOME/bat.installed");
    // A cargo copy of `bat` exists and the ownership record names cargo, but `bat` is still
    // declared (and resolves to the script install).
    let cargo = b.home.join(".cargo");
    let st = Command::new(cargo.join("bin/cargo"))
        .args(["install", "bat"])
        .env("HOME", &b.home)
        .env("CARGO_HOME", &cargo)
        .env("PATH", "/usr/bin:/bin")
        .status()
        .unwrap();
    assert!(st.success());
    let sp = b.home.join(".local/state/dots/state.json");
    let mut s: Value = serde_json::from_str(&std::fs::read_to_string(&sp).unwrap()).unwrap();
    s["packages"]["bat"]["provider"] = "cargo".into();
    s["packages"]["bat"]["name"] = "bat".into();
    write(&sp, &s.to_string());
    let path = format!("{}:/usr/bin:/bin", cargo.join("bin").display());
    let o = run_env(&b, &[("PATH", path.as_str())], &["bins", "--json"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let copies = json(&o)["data"]["bins"].as_array().unwrap().clone();
    let cargo_copy = copies
        .iter()
        .find(|c| c["provider"] == "cargo")
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                copies
                    .iter()
                    .filter(|c| c["bin"] == "bat")
                    .collect::<Vec<_>>()
            )
        });
    assert_ne!(cargo_copy["class"], "orphan", "{cargo_copy}");
}
