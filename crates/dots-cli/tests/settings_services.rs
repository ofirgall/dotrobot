//! `[[defaults]]` and `[service]` through the real binary on macOS, with fake `defaults`,
//! `killall` and `launchctl` in a tools directory (`DOTS_SYSTEM_TOOLS`). The fakes keep their
//! state in files and log every call, so nothing touches the machine. The Linux and Windows
//! backends are covered at the crate level (`dots-settings`, `dots-services`).
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    machine: PathBuf,
    tools: PathBuf,
    fake: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
}

#[cfg(unix)]
fn script(p: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(p, text);
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

const DEFAULTS: &str = r#"#!/bin/sh
D="$DOTS_FAKE_DIR/defaults"
echo "defaults $*" >> "$DOTS_FAKE_DIR/calls.log"
case "$1" in
  export)
    echo '<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>'
    if [ -d "$D/$2" ]; then
      for f in "$D/$2"/*; do
        [ -f "$f" ] || continue
        printf '<key>%s</key>' "$(basename "$f")"
        cat "$f"
      done
    fi
    echo '</dict></plist>' ;;
  write) mkdir -p "$D/$2"; printf '%s' "$4" > "$D/$2/$3" ;;
  delete) rm -f "$D/$2/$3" ;;
esac
"#;

const KILLALL: &str = r#"#!/bin/sh
echo "killall $*" >> "$DOTS_FAKE_DIR/calls.log"
"#;

const LAUNCHCTL: &str = r#"#!/bin/sh
echo "launchctl $*" >> "$DOTS_FAKE_DIR/calls.log"
case "$1" in
  print)
    if [ -f "$DOTS_FAKE_DIR/launchctl.print" ]; then cat "$DOTS_FAKE_DIR/launchctl.print"; else printf 'gui/501 = {\n\tservices = {\n\t}\n}\n'; fi ;;
  bootstrap)
    if [ -e "$3" ]; then echo "bootstrap unit-present" >> "$DOTS_FAKE_DIR/calls.log"; else echo "bootstrap unit-missing" >> "$DOTS_FAKE_DIR/calls.log"; fi ;;
esac
"#;

/// `pkgs`: (name, package.toml body). Every package is in bundle `all`.
fn sandbox(pkgs: &[(&str, &str)]) -> Sb {
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
    let tools = tmp.path().join("tools");
    script(&tools.join("defaults"), DEFAULTS);
    script(&tools.join("killall"), KILLALL);
    script(&tools.join("launchctl"), LAUNCHCTL);
    let fake = tmp.path().join("fake");
    std::fs::create_dir_all(&fake).unwrap();
    Sb {
        tmp,
        home,
        root,
        machine,
        tools,
        fake,
    }
}

impl Sb {
    fn state(&self) -> PathBuf {
        self.home.join(".local/state/dots")
    }

    fn seed(&self, domain: &str, key: &str, fragment: &str) {
        write(&self.fake.join("defaults").join(domain).join(key), fragment);
    }

    fn defaults(&self, domain: &str, key: &str) -> Option<String> {
        std::fs::read_to_string(self.fake.join("defaults").join(domain).join(key)).ok()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.fake.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn clear_calls(&self) {
        let _ = std::fs::remove_file(self.fake.join("calls.log"));
    }

    fn launchctl_print(&self, text: &str) {
        write(&self.fake.join("launchctl.print"), text);
    }

    fn package(&self, name: &str, body: &str) {
        write(
            &self.root.join(format!("packages/{name}/package.toml")),
            &format!("schema = 1\nname = \"{name}\"\n{body}"),
        );
    }
}

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run(b: &Sb, args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "script")
        .env("DOTS_SYSTEM_TOOLS", &b.tools)
        .env("DOTS_FAKE_DIR", &b.fake)
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
        code: o.status.code().unwrap_or(-1),
    }
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", o.stdout, o.stderr))
}

fn schema(command: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../schemas/dots-{command}.schema.json")),
        )
        .unwrap(),
    )
    .unwrap()
}

fn validate(command: &str, doc: &Value) {
    jsonschema::validator_for(&schema(command))
        .unwrap()
        .validate(doc)
        .unwrap_or_else(|e| panic!("{e}\n{doc}"));
}

fn items<'a>(doc: &'a Value, area: &str) -> Vec<&'a Value> {
    doc["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["area"] == area)
        .collect()
}

const KEYBOARD: &str = r#"
[[files]]
src = "files/keyboard.conf"
dst = "{home}/.config/keyboard/keyboard.conf"

[[defaults]]
domain = "NSGlobalDomain"
key = "KeyRepeat"
value = 2
restart = ["Dock"]

[service]
name = "com.example.autolock"
manager = "auto"
unit = "services/com.example.autolock.plist"
"#;

fn keyboard() -> Sb {
    let b = sandbox(&[("keyboard", KEYBOARD)]);
    write(
        &b.root.join("packages/keyboard/files/keyboard.conf"),
        "repeat=fast\n",
    );
    write(
        &b.root
            .join("packages/keyboard/services/com.example.autolock.plist"),
        "<plist/>\n",
    );
    b.seed("NSGlobalDomain", "KeyRepeat", "<integer>6</integer>");
    b
}

const RUNNING: &str =
    "gui/501 = {\n\tservices = {\n\t\t   620      0 \tcom.example.autolock\n\t}\n}\n";
const LOADED_STOPPED: &str =
    "gui/501 = {\n\tservices = {\n\t\t     -      0 \tcom.example.autolock\n\t}\n}\n";
const CRASHED: &str =
    "gui/501 = {\n\tservices = {\n\t\t     -      1 \tcom.example.autolock\n\t}\n}\n";

fn normalise(text: &str, b: &Sb) -> String {
    let mut s = text.replace(b.tmp.path().to_str().unwrap(), "<SB>");
    // `gui/<uid>`: the uid of whoever runs the tests
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("gui/") {
        out.push_str(&rest[..i + 4]);
        let tail = &rest[i + 4..];
        let n = tail.chars().take_while(char::is_ascii_digit).count();
        out.push_str(if n > 0 { "<uid>" } else { "" });
        rest = &tail[n..];
    }
    out.push_str(rest);
    s = out;
    s
}

fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var("DOTS_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        write(&path, actual);
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        expected,
        actual,
        "golden mismatch: {} (rerun with DOTS_UPDATE_GOLDEN=1 to update)",
        path.display()
    );
}

// ------------------------------------------------------------------ plan and apply

#[test]
fn plan_shows_the_setting_and_the_service_after_the_files() {
    let b = keyboard();
    let plain = run(&b, &["plan", "--color", "never"]);
    assert_eq!(plain.code, 0, "{}", plain.stderr);
    golden(
        "plan-settings-services.plain.txt",
        &normalise(&format!("exit: {}\n{}", plain.code, plain.stdout), &b),
    );
    let j = run(&b, &["plan", "--json"]);
    let doc = json(&j);
    validate("plan", &doc);
    golden(
        "plan-settings-services.json.txt",
        &normalise(&format!("exit: {}\n{}", j.code, j.stdout), &b),
    );
    let d = &doc["data"];
    assert_eq!(d["settings"][0]["detail"], "6 -> 2");
    assert_eq!(d["settings"][0]["id"], "defaults:NSGlobalDomain:KeyRepeat");
    assert_eq!(d["services"][0]["manager"], "launchd");
    assert_eq!(d["services"][0]["action"], "enable+start");
    // the unit is an owned file linked into LaunchAgents
    let files: Vec<&str> = d["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["dst"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&"~/Library/LaunchAgents/com.example.autolock.plist"),
        "{files:?}"
    );
    // plan reads, never writes
    assert!(b.calls().iter().all(|c| !c.starts_with("defaults write")));
    assert!(b.calls().iter().all(|c| !c.starts_with("killall")));
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>6</integer>"
    );
}

#[test]
fn apply_writes_the_config_then_the_setting_then_starts_the_service() {
    let b = keyboard();
    let o = run(&b, &["apply", "--yes", "--json"]);
    let doc = json(&o);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    validate("apply", &doc);
    assert_eq!(doc["data"]["summary"]["settings_ok"], 1);
    assert_eq!(doc["data"]["summary"]["services_ok"], 1);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>2</integer>"
    );
    assert!(b.home.join(".config/keyboard/keyboard.conf").exists());
    let calls = b.calls();
    let pos = |needle: &str| {
        calls
            .iter()
            .position(|c| c.contains(needle))
            .unwrap_or(usize::MAX)
    };
    // the service starts with its config (and unit) in place, after the setting
    assert!(
        pos("defaults write") < pos("launchctl bootstrap"),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"bootstrap unit-present".to_string()),
        "{calls:?}"
    );
    assert!(
        pos("launchctl bootstrap") < pos("launchctl kickstart"),
        "{calls:?}"
    );
    assert_eq!(calls.iter().filter(|c| *c == "killall Dock").count(), 1);
    // the original is in the ledger
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(b.state().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        ledger["owned"]["defaults:NSGlobalDomain:KeyRepeat"]["original"]["value"],
        6
    );
}

#[test]
fn a_converged_machine_has_no_setting_or_service_steps() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    b.clear_calls();
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["empty"], true, "{p}");
    assert_eq!(p["data"]["settings"].as_array().unwrap().len(), 0);
    assert_eq!(p["data"]["services"].as_array().unwrap().len(), 0);
    // read only: one export of the domain, one launchctl print
    let calls = b.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("defaults export"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("launchctl print"))
            .count(),
        1
    );
    assert!(calls.iter().all(|c| !c.starts_with("defaults write")));
}

// ------------------------------------------------------------------ status, drift, adopt

#[test]
fn status_shows_drift_and_adopt_writes_the_observed_value_back() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    // the user changes KeyRepeat in System Settings
    b.seed("NSGlobalDomain", "KeyRepeat", "<integer>6</integer>");
    let st = run(&b, &["status", "--json"]);
    let doc = json(&st);
    validate("status", &doc);
    assert_eq!(st.code, 2, "drift needs attention");
    let it = items(&doc, "settings");
    assert_eq!(it.len(), 1, "{doc}");
    assert_eq!(it[0]["state"], "drift");
    assert_eq!(it[0]["detail"], "observed 6, wanted 2");
    assert_eq!(
        it[0]["hints"][0],
        "dots adopt defaults:NSGlobalDomain:KeyRepeat"
    );

    let ad = run(
        &b,
        &["adopt", "defaults:NSGlobalDomain:KeyRepeat", "--json"],
    );
    assert_eq!(ad.code, 0, "{} {}", ad.stdout, ad.stderr);
    let toml = std::fs::read_to_string(b.root.join("packages/keyboard/package.toml")).unwrap();
    assert!(toml.contains("value = 6"), "{toml}");
    let st = run(&b, &["status", "--all", "--json"]);
    let doc = json(&st);
    assert_eq!(items(&doc, "settings")[0]["state"], "ok");
}

#[test]
fn apply_restores_the_declared_value_after_drift() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    b.seed("NSGlobalDomain", "KeyRepeat", "<integer>6</integer>");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>2</integer>"
    );
}

#[test]
fn an_xdg_entry_is_inert_on_macos() {
    let b = sandbox(&[(
        "md",
        "[[xdg]]\nmime = [\"text/markdown\"]\napp = \"x.desktop\"\n",
    )]);
    let doc = json(&run(&b, &["status", "--all", "--json"]));
    let it = items(&doc, "settings");
    assert_eq!(it[0]["state"], "n/a");
    assert_eq!(it[0]["detail"], "n/a on this platform");
    // and nothing is planned or executed for it
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["settings"].as_array().unwrap().len(), 0);
    assert!(b.calls().is_empty());
}

// ------------------------------------------------------------------ ownership and restore

#[test]
fn a_removed_declaration_restores_the_original_value() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>2</integer>"
    );
    // drop [[defaults]] from the package
    b.package(
        "keyboard",
        "[[files]]\nsrc = \"files/keyboard.conf\"\ndst = \"{home}/.config/keyboard/keyboard.conf\"\n",
    );
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["settings"][0]["action"], "restore");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>6</integer>"
    );
    // converged again
    assert_eq!(json(&run(&b, &["plan", "--json"]))["data"]["empty"], true);
}

#[test]
fn a_key_dots_created_is_deleted_when_its_declaration_goes() {
    let b = keyboard();
    std::fs::remove_file(b.fake.join("defaults/NSGlobalDomain/KeyRepeat")).unwrap();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(b.defaults("NSGlobalDomain", "KeyRepeat").is_some());
    b.package("keyboard", "");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(b.defaults("NSGlobalDomain", "KeyRepeat").is_none());
}

#[test]
fn restore_a_run_puts_the_settings_back() {
    let b = sandbox(&[(
        "two",
        "[[defaults]]\ndomain = \"NSGlobalDomain\"\nkey = \"KeyRepeat\"\nvalue = 2\n[[defaults]]\ndomain = \"com.apple.dock\"\nkey = \"autohide\"\nvalue = true\n",
    )]);
    b.seed("NSGlobalDomain", "KeyRepeat", "<integer>6</integer>");
    let ap = json(&run(&b, &["apply", "--yes", "--json"]));
    let id = ap["data"]["run"].as_str().unwrap().to_string();
    assert_eq!(ap["data"]["settings"].as_array().unwrap().len(), 2);
    let r = run(&b, &["restore", &id, "--json"]);
    assert_eq!(r.code, 0, "{} {}", r.stdout, r.stderr);
    let doc = json(&r);
    validate("restore", &doc);
    assert_eq!(doc["data"]["settings"].as_array().unwrap().len(), 2);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>6</integer>"
    );
    assert!(b.defaults("com.apple.dock", "autohide").is_none());
    // an unknown run is still an error
    let nope = run(&b, &["restore", "nope", "--json"]);
    assert_eq!(nope.code, 1);
    assert_eq!(json(&nope)["errors"][0]["code"], "backup-not-found");
}

// ------------------------------------------------------------------ restarts and logout

#[test]
fn restart_is_aggregated_across_packages() {
    let pkg = |k: &str| {
        format!(
            "[[defaults]]\ndomain = \"com.apple.dock\"\nkey = \"{k}\"\nvalue = true\nrestart = [\"Dock\"]\n"
        )
    };
    let b = sandbox(&[("a", &pkg("one")), ("b", &pkg("two")), ("c", &pkg("three"))]);
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let calls = b.calls();
    assert_eq!(
        calls.iter().filter(|c| *c == "killall Dock").count(),
        1,
        "{calls:?}"
    );
    // after the last change
    let last_write = calls
        .iter()
        .rposition(|c| c.starts_with("defaults write"))
        .unwrap();
    let kill = calls.iter().position(|c| c == "killall Dock").unwrap();
    assert!(kill > last_write);
    assert_eq!(json(&o)["data"]["restarts"][0]["app"], "Dock");
}

#[test]
fn needs_logout_raises_an_item_in_the_summary_and_the_state() {
    let b = sandbox(&[(
        "k",
        "[[defaults]]\ndomain = \"d\"\nkey = \"a\"\nvalue = 1\nrequires = [\"logout\"]\n",
    )]);
    let plain = run(&b, &["plan", "--color", "never"]);
    assert!(plain.stdout.contains("log out"), "{}", plain.stdout);
    let o = run(&b, &["apply", "--yes", "--json"]);
    let doc = json(&o);
    validate("apply", &doc);
    assert!(
        doc["data"]["todos"][0]["reason"]
            .as_str()
            .unwrap()
            .starts_with("logout:"),
        "{doc}"
    );
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(b.state().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(state["todos"][0]["package"], "k");
}

// ------------------------------------------------------------------ services

#[test]
fn a_linked_but_never_enabled_unit_shows_not_enabled_and_apply_enables_it() {
    let b = keyboard();
    // linked, loaded nowhere
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print("gui/501 = {\n\tservices = {\n\t}\n}\n");
    let doc = json(&run(&b, &["status", "--json"]));
    let it = items(&doc, "services");
    assert_eq!(it[0]["state"], "missing");
    assert_eq!(it[0]["detail"], "not enabled");
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(
        b.calls()
            .iter()
            .any(|c| c.starts_with("launchctl bootstrap"))
    );
}

#[test]
fn already_running_means_no_service_step() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    let doc = json(&run(&b, &["status", "--all", "--json"]));
    assert_eq!(items(&doc, "services")[0]["state"], "ok");
    assert_eq!(items(&doc, "services")[0]["have"], "running");
}

#[test]
fn a_failed_service_is_reported_and_the_package_is_not_ok() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(CRASHED);
    let st = run(&b, &["status", "--json"]);
    let doc = json(&st);
    assert_eq!(st.code, 2);
    let it = items(&doc, "services");
    assert_eq!(it[0]["state"], "failed");
    assert_eq!(it[0]["have"], "failed");
}

#[test]
fn one_launchctl_call_reads_all_services() {
    let svc = |n: &str| {
        format!(
            "[service]\nname = \"com.example.{n}\"\nmanager = \"launchd\"\nunit = \"services/com.example.{n}.plist\"\n"
        )
    };
    let b = sandbox(&[
        ("a", &svc("a")),
        ("b", &svc("b")),
        ("c", &svc("c")),
        ("d", &svc("d")),
    ]);
    for n in ["a", "b", "c", "d"] {
        write(
            &b.root
                .join(format!("packages/{n}/services/com.example.{n}.plist")),
            "<plist/>",
        );
    }
    let doc = json(&run(&b, &["status", "--all", "--json"]));
    assert_eq!(items(&doc, "services").len(), 4);
    assert_eq!(
        b.calls()
            .iter()
            .filter(|c| c.starts_with("launchctl print"))
            .count(),
        1,
        "{:?}",
        b.calls()
    );
}

#[test]
fn two_changed_config_files_restart_the_running_service_once() {
    let b = sandbox(&[(
        "svc",
        r#"
[[files]]
src = "files/one.conf"
dst = "{home}/.config/svc/one.conf"
[[files]]
src = "files/two.conf"
dst = "{home}/.config/svc/two.conf"
[service]
name = "com.example.autolock"
manager = "launchd"
unit = "services/com.example.autolock.plist"
"#,
    )]);
    write(&b.root.join("packages/svc/files/one.conf"), "1\n");
    write(&b.root.join("packages/svc/files/two.conf"), "2\n");
    write(
        &b.root
            .join("packages/svc/services/com.example.autolock.plist"),
        "<plist/>",
    );
    // converge once
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    // now both configs change in the repo
    write(&b.root.join("packages/svc/files/one.conf"), "1b\n");
    write(&b.root.join("packages/svc/files/two.conf"), "2b\n");
    // (links follow the repo, so change them to copies' worth of change: render-free check)
    b.clear_calls();
    let p = json(&run(&b, &["plan", "--json"]));
    // a linked source never produces a files step, but its content changed: the service
    // restarts once, and that is now the baseline
    assert_eq!(p["data"]["services"].as_array().unwrap().len(), 1, "{p}");
    assert_eq!(p["data"]["services"][0]["action"], "restart", "{p}");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["services"].as_array().unwrap().len(), 0, "{p}");
    b.package(
        "svc",
        r#"
[[files]]
mode = "copy"
src = "files/one.conf"
dst = "{home}/.config/svc/one.conf"
[[files]]
mode = "copy"
src = "files/two.conf"
dst = "{home}/.config/svc/two.conf"
[service]
name = "com.example.autolock"
manager = "launchd"
unit = "services/com.example.autolock.plist"
"#,
    );
    let plan = json(&run(&b, &["plan", "--json"]));
    let services = plan["data"]["services"].as_array().unwrap();
    assert_eq!(services.len(), 1, "{plan}");
    assert_eq!(services[0]["action"], "restart");
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let calls = b.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("launchctl bootout"))
            .count(),
        1,
        "{calls:?}"
    );
    // nothing changed on the next run: no restart
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(
        b.calls().iter().all(|c| !c.contains("kickstart")),
        "{:?}",
        b.calls()
    );
}

#[test]
fn a_service_that_is_not_running_is_not_started_by_a_config_change_unless_asked() {
    let body = |running: bool| {
        format!(
            r#"
[[files]]
mode = "copy"
src = "files/one.conf"
dst = "{{home}}/.config/svc/one.conf"
[service]
name = "com.example.autolock"
manager = "launchd"
running = {running}
unit = "services/com.example.autolock.plist"
"#
        )
    };
    let b = sandbox(&[("svc", &body(false))]);
    write(&b.root.join("packages/svc/files/one.conf"), "1\n");
    write(
        &b.root
            .join("packages/svc/services/com.example.autolock.plist"),
        "<plist/>",
    );
    // loaded and enabled, but stopped
    b.launchctl_print(LOADED_STOPPED);
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    write(&b.root.join("packages/svc/files/one.conf"), "2\n");
    let plan = json(&run(&b, &["plan", "--json"]));
    assert_eq!(
        plan["data"]["services"].as_array().unwrap().len(),
        0,
        "{plan}"
    );
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(
        b.calls().iter().all(|c| !c.contains("kickstart")),
        "{:?}",
        b.calls()
    );
    // with running = true it is started
    b.package("svc", &body(true));
    let plan = json(&run(&b, &["plan", "--json"]));
    assert_eq!(plan["data"]["services"][0]["action"], "start", "{plan}");
}

// ------------------------------------------------------------------ review fixes

fn real_uid() -> String {
    let o = Command::new("/usr/bin/id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn run_with_home(b: &Sb, home: &Path, args: &[&str]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "script")
        .env("DOTS_SYSTEM_TOOLS", &b.tools)
        .env("DOTS_FAKE_DIR", &b.fake)
        .env("HOME", b.tmp.path().join("sentinel"))
        .current_dir(b.tmp.path())
        .arg("--home")
        .arg(home)
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

#[test]
fn dropping_a_package_restores_its_settings() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>2</integer>"
    );
    // the package stops applying here (`when` false): it is in the repo, but not active
    b.package("keyboard", &format!("when = \"false\"\n{KEYBOARD}"));
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["settings"][0]["action"], "restore", "{p}");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>6</integer>"
    );
}

#[test]
fn dropping_a_package_takes_its_enabled_service_down() {
    let b = keyboard();
    // first run: the job is not loaded, so dots enables and starts it
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(b.state().join("services.json").is_file());
    b.launchctl_print(RUNNING);
    b.package("keyboard", "when = \"false\"\n");
    let p = json(&run(&b, &["plan", "--json"]));
    let svc = p["data"]["services"].as_array().unwrap();
    assert_eq!(svc.len(), 1, "{p}");
    assert_eq!(svc[0]["action"], "disable");
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let uid = real_uid();
    assert!(
        b.calls()
            .contains(&format!("launchctl bootout gui/{uid}/com.example.autolock")),
        "{:?}",
        b.calls()
    );
    // forgotten: the next plan has nothing to do
    b.launchctl_print("gui/501 = {\n\tservices = {\n\t}\n}\n");
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["services"].as_array().unwrap().len(), 0, "{p}");
}

#[test]
fn a_service_that_was_already_enabled_is_not_disabled_when_its_package_goes() {
    let b = keyboard();
    b.launchctl_print(RUNNING);
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.package("keyboard", "when = \"false\"\n");
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["services"].as_array().unwrap().len(), 0, "{p}");
}

#[test]
fn launchd_bootstraps_before_it_kickstarts() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let calls = b.calls();
    let at = |p: &str| calls.iter().position(|c| c.starts_with(p));
    let (boot, kick) = (at("launchctl bootstrap"), at("launchctl kickstart"));
    assert!(boot.is_some() && kick.is_some(), "{calls:?}");
    assert!(boot < kick, "{calls:?}");
}

#[test]
fn a_changed_unit_is_booted_out_and_in_not_kicked() {
    let b = keyboard();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    // the plist in the repo changes; it is linked, so there is no files step for it
    write(
        &b.root
            .join("packages/keyboard/services/com.example.autolock.plist"),
        "<plist>new</plist>\n",
    );
    let p = json(&run(&b, &["plan", "--json"]));
    assert_eq!(p["data"]["services"][0]["action"], "restart", "{p}");
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let calls = b.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("launchctl bootout")),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("launchctl bootstrap")),
        "{calls:?}"
    );
    assert!(
        calls.iter().all(|c| !c.contains("kickstart -k")),
        "{calls:?}"
    );
    // converged: the same config restarts nothing
    b.clear_calls();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(
        b.calls().iter().all(|c| !c.contains("bootout")),
        "{:?}",
        b.calls()
    );
}

#[test]
fn a_hostile_service_name_is_refused_at_plan_time() {
    let b = sandbox(&[(
        "evil",
        "[service]\nname = \"x & calc\"\nmanager = \"launchd\"\n",
    )]);
    let o = run(&b, &["plan", "--json"]);
    assert_ne!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let text = format!("{}{}", o.stdout, o.stderr);
    assert!(text.contains("is not allowed"), "{text}");
    assert!(
        b.calls().iter().all(|c| !c.contains("calc")),
        "{:?}",
        b.calls()
    );
}

#[test]
fn the_launchd_domain_is_the_real_users_not_the_homes_owner() {
    let b = keyboard();
    // a home owned by root: the old code took its owner for the user
    let o = run_with_home(
        &b,
        Path::new("/private/var/empty"),
        &["status", "--all", "--json"],
    );
    let _ = o;
    let uid = real_uid();
    let prints: Vec<String> = b
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("launchctl print"))
        .collect();
    assert!(!prints.is_empty(), "{:?}", b.calls());
    for p in prints {
        assert_eq!(p, format!("launchctl print gui/{uid}"));
    }
}

#[test]
fn plan_marks_sudo_steps_and_says_what_plain_apply_does() {
    let b = sandbox(&[(
        "ua",
        "[[defaults]]\ndomain = \"com.apple.universalaccess\"\nkey = \"reduceMotion\"\nvalue = true\nroot = true\n",
    )]);
    let o = run(&b, &["plan"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let all = format!("{}{}", o.stdout, o.stderr);
    assert!(all.contains("(sudo)"), "{all}");
    assert!(
        all.contains("1 step needs sudo, so a plain `dots apply` skips it")
            && all.contains("--with-sudo")
            && all.contains("--sudo-only"),
        "{all}"
    );
    assert!(!all.contains("Needs your password once"), "{all}");
    let j = run(&b, &["plan", "--json"]);
    let v: Value = serde_json::from_str(&j.stdout).unwrap();
    assert_eq!(v["data"]["summary"]["sudo_steps"], 1);
    // `--with-sudo` and `--sudo-only` are exclusive.
    let both = run(&b, &["apply", "--with-sudo", "--sudo-only"]);
    assert_ne!(both.code, 0);
    assert!(
        both.stderr.contains("cannot be used with"),
        "{}",
        both.stderr
    );
}

#[test]
fn a_root_setting_without_sudo_fails_and_never_runs_as_the_user() {
    let b = sandbox(&[(
        "ua",
        "[[defaults]]\ndomain = \"com.apple.universalaccess\"\nkey = \"reduceMotion\"\nvalue = true\nroot = true\n",
    )]);
    // no sudo on this machine: nothing primes it
    write(
        &b.machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n[facts]\nsudo = false\n",
            b.root.display().to_string()
        ),
    );
    // A plain apply never runs a root step: it warns and exits 0.
    let plain = run(&b, &["apply", "--yes"]);
    assert_eq!(plain.code, 0, "{} {}", plain.stdout, plain.stderr);
    assert!(
        plain
            .stdout
            .contains("1 step needs sudo and was not applied")
            && plain.stdout.contains("dots apply --sudo-only"),
        "{}",
        plain.stdout
    );
    // `--with-sudo` tries, and fails because there is no sudo.
    let o = run(&b, &["apply", "--yes", "--with-sudo"]);
    assert_ne!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert!(
        b.calls().iter().all(|c| !c.starts_with("defaults write")),
        "{:?}",
        b.calls()
    );
    assert!(
        b.defaults("com.apple.universalaccess", "reduceMotion")
            .is_none()
    );
}

#[test]
fn adopt_edits_the_override_that_set_the_value() {
    let b = keyboard();
    write(
        &b.root.join("overrides/packages/keyboard/override.toml"),
        "schema = 1\n[[defaults]]\ndomain = \"NSGlobalDomain\"\nkey = \"KeyRepeat\"\nvalue = 3\n",
    );
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    // the override wins: 3 was written; now the user picks 5 in System Settings
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>3</integer>"
    );
    b.seed("NSGlobalDomain", "KeyRepeat", "<integer>5</integer>");
    let ad = run(
        &b,
        &["adopt", "defaults:NSGlobalDomain:KeyRepeat", "--json"],
    );
    assert_eq!(ad.code, 0, "{} {}", ad.stdout, ad.stderr);
    let pkg = std::fs::read_to_string(b.root.join("packages/keyboard/package.toml")).unwrap();
    assert!(
        pkg.contains("value = 2"),
        "package.toml is untouched: {pkg}"
    );
    let ov =
        std::fs::read_to_string(b.root.join("overrides/packages/keyboard/override.toml")).unwrap();
    assert!(ov.contains("value = 5"), "{ov}");
    let doc = json(&run(&b, &["status", "--all", "--json"]));
    assert_eq!(items(&doc, "settings")[0]["state"], "ok", "{doc}");
}

#[test]
fn adopt_edits_the_platform_table_that_set_the_value() {
    let b = keyboard();
    let pkg = b.root.join("packages/keyboard/package.toml");
    let mut text = std::fs::read_to_string(&pkg).unwrap();
    text.push_str(
        "\n[[platform.macos.defaults]]\ndomain = \"NSGlobalDomain\"\nkey = \"KeyRepeat\"\nvalue = 4\n",
    );
    std::fs::write(&pkg, text).unwrap();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    b.launchctl_print(RUNNING);
    assert_eq!(
        b.defaults("NSGlobalDomain", "KeyRepeat").unwrap(),
        "<integer>4</integer>"
    );
    b.seed("NSGlobalDomain", "KeyRepeat", "<integer>6</integer>");
    let ad = run(
        &b,
        &["adopt", "defaults:NSGlobalDomain:KeyRepeat", "--json"],
    );
    assert_eq!(ad.code, 0, "{} {}", ad.stdout, ad.stderr);
    let after = std::fs::read_to_string(&pkg).unwrap();
    assert!(after.contains("value = 2"), "the base value stays: {after}");
    assert!(after.contains("value = 6"), "{after}");
    let doc = json(&run(&b, &["status", "--all", "--json"]));
    assert_eq!(items(&doc, "settings")[0]["state"], "ok", "{doc}");
}

#[test]
fn a_hostile_service_name_keeps_its_error_code_in_the_json_errors() {
    let b = sandbox(&[(
        "evil",
        "[service]\nname = \"x & calc\"\nmanager = \"launchd\"\n",
    )]);
    for cmd in ["plan", "apply"] {
        let args: Vec<&str> = if cmd == "apply" {
            vec!["apply", "--yes", "--json"]
        } else {
            vec!["plan", "--json"]
        };
        let o = run(&b, &args);
        assert_ne!(o.code, 0, "{} {}", o.stdout, o.stderr);
        let doc = json(&o);
        let codes: Vec<&str> = doc["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["service-name-invalid"], "{cmd}: {doc}");
    }
}

#[test]
fn status_of_a_root_setting_without_a_cached_sudo_is_unknown_with_a_hint() {
    let b = sandbox(&[(
        "ua",
        "[[defaults]]\ndomain = \"com.apple.universalaccess\"\nkey = \"reduceMotion\"\nvalue = true\nroot = true\n",
    )]);
    // `sudo -n` refuses: no credential is cached
    script(
        &b.tools.join("sudo"),
        "#!/bin/sh\necho 'sudo: a password is required' >&2\nexit 1\n",
    );
    let o = run(&b, &["status", "--all", "--json"]);
    let doc = json(&o);
    let s = items(&doc, "settings");
    assert_eq!(s.len(), 1, "{doc}");
    assert_eq!(s[0]["state"], "unknown", "{doc}");
    let detail = s[0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("needs sudo to read") && detail.contains("sudo -v"),
        "{detail}"
    );
    // Bug 35: it waits on sudo, and the hint is the command that runs it (a plain apply skips it)
    assert_eq!(s[0]["sudo"], "root", "{doc}");
    assert_eq!(
        s[0]["hints"],
        serde_json::json!(["dots apply --with-sudo ua"]),
        "{doc}"
    );
    let plain = run(&b, &["status", "--all", "--color", "never"]);
    assert!(plain.stdout.contains("unknown"), "{}", plain.stdout);
    assert!(
        plain.stdout.contains("1 waits on sudo"),
        "the summary says so: {}",
        plain.stdout
    );
}

#[test]
fn plan_calls_out_a_reboot_requirement() {
    let b = sandbox(&[(
        "k",
        "[[defaults]]\ndomain = \"d\"\nkey = \"a\"\nvalue = 1\nrequires = [\"reboot\"]\n",
    )]);
    let plain = run(&b, &["plan", "--color", "never"]);
    assert!(plain.stdout.contains("after a reboot"), "{}", plain.stdout);
    assert!(!plain.stdout.contains("log out"), "{}", plain.stdout);
}

#[test]
fn a_logout_todo_closes_once_the_setting_reads_as_declared() {
    let b = sandbox(&[(
        "k",
        "[[defaults]]\ndomain = \"d\"\nkey = \"a\"\nvalue = 1\nrequires = [\"logout\"]\n",
    )]);
    let doc = json(&run(&b, &["apply", "--yes", "--json"]));
    assert_eq!(doc["data"]["todos"].as_array().unwrap().len(), 1, "{doc}");
    let todos = |b: &Sb| -> usize {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(b.state().join("state.json")).unwrap())
                .unwrap();
        v["todos"].as_array().unwrap().len()
    };
    assert_eq!(todos(&b), 1);
    // the setting is in place (the apply wrote it), so status already hides the todo
    let st = json(&run(&b, &["status", "--all", "--json"]));
    assert!(items(&st, "todos").is_empty(), "{st}");
    // and the next apply, with nothing else to do, drops it from the state
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(todos(&b), 0);
}
