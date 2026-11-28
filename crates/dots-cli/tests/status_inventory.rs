//! Task 4.8: every `status-inventory` scenario through the real binary in a `--home` sandbox
//! with script packages (no package manager is touched), plus plain and JSON goldens and the
//! 100-package benchmark.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))] // helpers of unix-only tests

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    machine: PathBuf,
}

impl Sb {
    fn cache(&self) -> PathBuf {
        self.home.join(".cache/dots/latest")
    }
    fn state(&self) -> PathBuf {
        self.home.join(".local/state/dots")
    }
    fn local_bin(&self) -> PathBuf {
        self.home.join(".local/bin")
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
}

fn exe(p: &Path) {
    write(p, "#!/bin/sh\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A script package: installed when `$HOME/<name>.installed` exists, `1.0.0` installed, the
/// provider's latest is `latest`. `extra` goes before the provider table.
fn script(name: &str, latest: &str, extra: &str) -> (String, String) {
    (
        name.to_string(),
        format!(
            "{extra}\n[providers.script]\ncheck = \"test -f $HOME/{name}.installed\"\nversion = \"echo 1.0.0\"\nlatest = \"echo {latest}\"\ninstall = \"touch $HOME/{name}.installed\"\n"
        ),
    )
}

/// Packages (name, package.toml body after the header), all in bundle `all`. `machine_extra`
/// is appended to machine.toml.
fn sandbox(pkgs: &[(String, String)], machine_extra: &str) -> Sb {
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
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n{machine_extra}",
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

fn cmd(b: &Sb, path: &str, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dots"));
    c.env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", path)
        .env("SHELL", "/bin/zsh")
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

fn run_path(b: &Sb, path: &str, args: &[&str]) -> Out {
    let o = cmd(b, path, args).stdin(Stdio::null()).output().unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn run(b: &Sb, args: &[&str]) -> Out {
    run_path(b, &dots_testkit::system_path(), args)
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

fn installed(b: &Sb, names: &[&str]) {
    for n in names {
        write(&b.home.join(format!("{n}.installed")), "");
    }
}

/// Write `<cache>/latest/script.json` as if `outdated` had just run.
fn seed_cache(b: &Sb, fetched: u64, latest: &[(&str, &str)]) {
    let versions: serde_json::Map<String, Value> = latest
        .iter()
        .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
        .collect();
    write(
        &b.cache().join("script.json"),
        &serde_json::json!({ "schema": 1, "fetched": fetched, "versions": versions }).to_string(),
    );
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

const FILES_PKG: &str = "[[files]]\nsrc = \"files/aliases\"\ndst = \"{home}/.aliases\"\n";

/// Three outdated script packages, one healthy, and one file package with a modified file.
fn attention_sandbox() -> Sb {
    let mut pkgs: Vec<(String, String)> = ["o1", "o2", "o3"]
        .iter()
        .map(|n| script(n, "2.0.0", ""))
        .collect();
    pkgs.push(script("fine", "1.0.0", ""));
    pkgs.push((
        "shellcfg".into(),
        "[[files]]\nmode = \"copy\"\nsrc = \"files/aliases\"\ndst = \"{home}/.aliases\"\n".into(),
    ));
    let b = sandbox(&pkgs, "");
    write(
        &b.root.join("packages/shellcfg/files/aliases"),
        "alias ll='ls -l'\n",
    );
    installed(&b, &["o1", "o2", "o3", "fine"]);
    let o = run(&b, &["apply", "--yes", "--files-only"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    seed_cache(
        &b,
        now(),
        &[
            ("o1", "2.0.0"),
            ("o2", "2.0.0"),
            ("o3", "2.0.0"),
            ("fine", "1.0.0"),
        ],
    );
    // The app edits the copy.
    std::fs::write(b.home.join(".aliases"), "alias ll='ls -lah'\n").unwrap();
    b
}

// ------------------------------------------------------------------ status

#[test]
fn attention_summary_lists_only_the_problem_rows() {
    let b = attention_sandbox();
    let o = run(&b, &["status"]);
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("4 things need attention"), "{}", o.stdout);
    for row in ["o1", "o2", "o3", "~/.aliases"] {
        assert!(o.stdout.contains(row), "{row} missing: {}", o.stdout);
    }
    assert!(o.stdout.contains("modified"), "{}", o.stdout);
    assert!(
        o.stdout.contains("dots adopt"),
        "fix hint for the modified file: {}",
        o.stdout
    );
    assert!(o.stdout.contains("dots upgrade o1"), "{}", o.stdout);
    // healthy rows are counted, not listed
    assert!(
        !o.stdout.contains("[ok]") && !o.stdout.contains(" fine "),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("2 ok"), "{}", o.stdout);

    let j = run(&b, &["status", "--json"]);
    assert_eq!(j.code, 2);
    let d = json(&j);
    validate("status", &d);
    assert_eq!(d["schema"], "dots/status@1");
    assert_eq!(d["data"]["attention"], 4);
    assert_eq!(d["data"]["counts"]["packages"]["outdated"], 3);
    assert_eq!(d["data"]["counts"]["files"]["modified"], 1);
    let items = d["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "{d}");
    assert!(items.iter().all(|i| i["state"] != "ok"));
}

#[test]
fn a_healthy_machine_collapses_to_one_line_and_exits_0() {
    let pkgs = vec![script("a", "1.0.0", ""), script("b", "1.0.0", "")];
    let b = sandbox(&pkgs, "");
    installed(&b, &["a", "b"]);
    let o = run(&b, &["status"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("All good. 2 packages"), "{}", o.stdout);
    assert!(!o.stdout.contains("PACKAGES"), "{}", o.stdout);
    let d = json(&run(&b, &["status", "--json"]));
    validate("status", &d);
    assert_eq!(d["data"]["attention"], 0);
    assert_eq!(d["data"]["items"].as_array().unwrap().len(), 0);
    // bare `dots` is the same screen (its badge reads `dots`, not `dots status`)
    let bare = run(&b, &[]);
    assert_eq!(bare.code, 0);
    assert_eq!(
        bare.stdout.replacen(" dots\n", " dots status\n", 1),
        o.stdout
    );
}

#[test]
fn bare_dots_groups_attention_by_kind_and_exits_like_status() {
    let b = attention_sandbox();
    let o = run(&b, &[]);
    assert_eq!(o.code, run(&b, &["status"]).code);
    // 3 outdated packages are one line, the modified file another.
    assert!(o.stdout.contains("2 things need attention"), "{}", o.stdout);
    assert!(
        o.stdout.contains("3 updates available") && o.stdout.contains("o1, o2, o3"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("1 config changed by app"), "{}", o.stdout);
    assert!(o.stdout.contains("-> dots upgrade"), "{}", o.stdout);
    assert!(
        o.stdout.contains("-> dots adopt ~/.aliases"),
        "{}",
        o.stdout
    );
}

#[test]
#[cfg(unix)]
fn all_includes_inert_and_excluded_rows() {
    let pkgs = vec![
        script("a", "1.0.0", ""),
        script("winonly", "1.0.0", "when = 'platform = \"windows\"'"),
        script("skipped", "1.0.0", ""),
    ];
    let b = sandbox(&pkgs, "exclude = [\"skipped\"]\n");
    installed(&b, &["a"]);
    let plain = run(&b, &["status"]);
    assert_eq!(plain.code, 0, "{}{}", plain.stdout, plain.stderr);
    assert!(!plain.stdout.contains("winonly") && !plain.stdout.contains("skipped"));
    let o = run(&b, &["status", "--all"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("winonly") && o.stdout.contains("n/a on this platform"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("skipped") && o.stdout.contains("excluded"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("[ok]"),
        "ok rows are listed too: {}",
        o.stdout
    );
    let d = json(&run(&b, &["status", "--all", "--json"]));
    validate("status", &d);
    assert_eq!(d["data"]["view"], "all");
    let states: Vec<&str> = d["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    assert!(states.contains(&"n/a") && states.contains(&"excluded") && states.contains(&"ok"));
}

#[test]
fn deep_view_shows_provider_versions_files_todos_and_history() {
    let mut pkg = script("tool", "1.2.0", "description = \"A tool\"\n");
    pkg.1.push_str(FILES_PKG);
    pkg.1
        .push_str("[env]\nvars = { TOOL_HOME = \"/opt/tool\" }\n");
    let b = sandbox(&[pkg], "");
    write(&b.root.join("packages/tool/files/aliases"), "x\n");
    write(
        &b.root.join("overrides/packages/tool/override.toml"),
        "schema = 1\n",
    );
    installed(&b, &["tool"]);
    seed_cache(&b, now(), &[("tool", "1.2.0")]);
    write(
        &b.state().join("state.json"),
        r#"{"schema":1,"packages":{},"files":{},"todos":[{"package":"tool","reason":"Grant access","run":"r-1"}]}"#,
    );
    write(
        &b.state().join("journal.jsonl"),
        "{\"ts\":\"2026-11-20T10:00:00Z\",\"run\":\"r-1\",\"op\":\"install\",\"pkg\":\"tool\",\"provider\":\"script\",\"from\":null,\"to\":\"1.0.0\",\"exit\":0}\n",
    );
    let o = run(&b, &["status", "tool"]);
    assert_eq!(
        o.code, 2,
        "outdated + missing file: {}{}",
        o.stdout, o.stderr
    );
    for want in [
        "A tool",
        "provider: script",
        "script chosen",
        "installed  1.0.0",
        "latest     1.2.0",
        "~/.aliases",
        "missing",
        "Grant access",
        "TOOL_HOME",
        "override.toml",
        "2026-11-20  install 1.0.0",
        "machine -> all -> tool",
    ] {
        assert!(o.stdout.contains(want), "`{want}` missing:\n{}", o.stdout);
    }
    let d = json(&run(&b, &["status", "tool", "--json"]));
    validate("status", &d);
    let data = &d["data"];
    assert_eq!(data["view"], "package");
    assert_eq!(data["state"], "outdated");
    assert_eq!(data["versions"]["installed"], "1.0.0");
    assert_eq!(data["files"][0]["state"], "missing");
    assert_eq!(data["todos"][0]["detail"], "Grant access");
    assert_eq!(data["history"][0]["op"], "install");
    assert_eq!(data["overrides"][0]["scope"], "repo");
    // unknown package
    let bad = run(&b, &["status", "nope"]);
    assert_eq!(bad.code, 1);
    assert!(bad.stderr.contains("unknown-package"), "{}", bad.stderr);
}

#[test]
fn strict_makes_version_drift_an_error() {
    let b = sandbox(&[script("pinned", "1.0.0", "version = \"2.0.0\"")], "");
    installed(&b, &["pinned"]);
    let o = run(&b, &["status"]);
    assert_eq!(o.code, 2, "{}", o.stdout);
    assert!(
        o.stdout.contains("drift") && o.stdout.contains("wants 2.0.0"),
        "{}",
        o.stdout
    );
    assert_eq!(run(&b, &["status", "--strict"]).code, 1);
}

/// Bug 34: a lock row older than what is installed (a provider that cannot install an older
/// version, so the plan has no step) is shown with a hint that works: `dots apply` records the
/// installed version, and the drift is gone. Status and plan agree throughout.
#[test]
fn a_stale_lock_row_is_drift_that_one_apply_clears() {
    let b = sandbox(&[script("tool", "1.0.0", "")], "");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let lock = b.root.join("dots.lock");
    let text = std::fs::read_to_string(&lock).unwrap();
    assert!(text.contains("1.0.0"), "{text}");
    std::fs::write(&lock, text.replace("1.0.0", "0.9.0")).unwrap();

    let o = run(&b, &["status"]);
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("dots apply tool"), "{}", o.stdout);
    let d = json(&run(&b, &["status", "--json"]));
    let detail = d["data"]["items"][0]["detail"].as_str().unwrap();
    assert!(
        detail.contains("locked 0.9.0, installed 1.0.0") && detail.contains("updates the lock"),
        "{detail}"
    );
    // the plan has no package step, but it is not blind to the row: apply acts on it
    let plan = json(&run(&b, &["plan", "--json"]));
    assert_eq!(plan["data"]["summary"]["packages"], 0, "{plan}");
    assert_eq!(plan["data"]["lock"][0]["rewrite"], true, "{plan}");
    assert_eq!(plan["data"]["lock"][0]["to"], "1.0.0", "{plan}");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert!(
        std::fs::read_to_string(&lock).unwrap().contains("1.0.0"),
        "the row follows what is installed"
    );
    let after = run(&b, &["status"]);
    assert_eq!(after.code, 0, "{}{}", after.stdout, after.stderr);
}

/// Bug 39: a package adopted from outside dots whose lock row lags (the newer version is what is
/// installed and satisfies `latest`) was drift with a hint that did nothing: the plan said
/// "Nothing to do" and apply left the row. The plan now lists a lock-only update and apply
/// rewrites `dots.lock` without installing anything.
#[test]
fn a_stale_lock_row_of_an_adopted_package_is_a_lock_only_update() {
    let b = sandbox(&[script("tool", "1.0.0", "")], "");
    installed(&b, &["tool"]);
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let lock = b.root.join("dots.lock");
    let text = std::fs::read_to_string(&lock).unwrap();
    assert!(text.contains("1.0.0"), "{text}");
    std::fs::write(&lock, text.replace("1.0.0", "0.9.0")).unwrap();

    let st = json(&run(&b, &["status", "--json"]));
    let item = &st["data"]["items"][0];
    assert_eq!(item["state"], "drift", "{st}");
    assert_eq!(item["hints"][0], "dots apply tool", "{st}");

    let plan = run(&b, &["plan"]);
    assert!(
        plan.stdout.contains("Lock") && !plan.stdout.contains("Nothing to do"),
        "{}",
        plan.stdout
    );
    let pj = json(&run(&b, &["plan", "--json"]));
    assert_eq!(pj["data"]["empty"], false, "{pj}");
    assert_eq!(pj["data"]["summary"]["packages"], 0, "{pj}");
    assert_eq!(pj["data"]["lock"][0]["to"], "1.0.0", "{pj}");

    let o = run(&b, &["apply", "--yes"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(std::fs::read_to_string(&lock).unwrap().contains("1.0.0"));
    let after = run(&b, &["status"]);
    assert_eq!(after.code, 0, "{}{}", after.stdout, after.stderr);
    assert!(run(&b, &["plan"]).stdout.contains("Nothing to do"));
}

/// Bug 39: a package whose version the provider cannot read (a script with no `version`
/// command) is ok when nothing else is wrong, whatever its lock row says; plan and apply leave
/// the row alone.
#[test]
fn an_unknown_installed_version_is_not_drift() {
    let b = sandbox(&[script("tool", "1.0.0", "")], "");
    installed(&b, &["tool"]);
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let lock = b.root.join("dots.lock");
    let text = std::fs::read_to_string(&lock).unwrap();
    let locked = text.replace("1.0.0", "2026.11.22-e9042b2");
    std::fs::write(&lock, &locked).unwrap();
    let pkg = b.root.join("packages/tool/package.toml");
    let body = std::fs::read_to_string(&pkg).unwrap();
    std::fs::write(
        &pkg,
        body.replace("version = \"echo 1.0.0\"", "version = \"true\""),
    )
    .unwrap();

    let st = json(&run(&b, &["status", "--all", "--json"]));
    let item = &st["data"]["items"][0];
    assert_eq!(item["have"], "unknown", "{st}");
    assert_eq!(item["state"], "ok", "{st}");
    assert!(
        item["detail"].as_str().unwrap().contains("version unknown"),
        "{st}"
    );
    assert!(
        run(&b, &["plan"]).stdout.contains("Nothing to do"),
        "plan agrees"
    );
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), locked);
}

/// Bug 35: a package that waits on sudo says so, and hints the command that runs it (a plain
/// `dots apply` skips it).
#[test]
fn a_package_waiting_on_sudo_is_badged_and_hints_with_sudo() {
    let b = sandbox(
        &[
            script("warp", "1.0.0", "requires_sudo = true"),
            script("plain", "1.0.0", ""),
        ],
        "",
    );
    let o = run(&b, &["status", "--color", "never"]);
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("needs sudo") && o.stdout.contains("dots apply --with-sudo warp"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("2 things need attention (1 wait on sudo)"),
        "{}",
        o.stdout
    );
    // the ordinary one keeps the plain hint
    assert!(o.stdout.contains("dots apply plain"), "{}", o.stdout);
    assert!(!o.stdout.contains("--with-sudo plain"), "{}", o.stdout);

    let d = json(&run(&b, &["status", "--json"]));
    validate("status", &d);
    let items = d["data"]["items"].as_array().unwrap();
    let warp = items.iter().find(|i| i["name"] == "warp").unwrap();
    assert_eq!(warp["sudo"], "requires_sudo", "{d}");
    let plain = items.iter().find(|i| i["name"] == "plain").unwrap();
    assert!(plain.get("sudo").is_none(), "{d}");

    // the hint is real: it is the command that installs it
    let w = run(&b, &["apply", "--with-sudo", "warp", "--yes", "--dry-run"]);
    assert_eq!(w.code, 0, "{}{}", w.stdout, w.stderr);
    assert!(w.stdout.contains("warp"), "{}", w.stdout);
}

// ------------------------------------------------------------------ the network

/// A package whose `latest` leaves a marker file when it runs.
fn marker_sandbox() -> (Sb, PathBuf) {
    let pkgs = vec![(
        "net".to_string(),
        "[providers.script]\ncheck = \"test -f $HOME/net.installed\"\nversion = \"echo 1.0.0\"\nlatest = \"touch $HOME/latest.called; echo 2.0.0\"\ninstall = \"true\"\n"
            .to_string(),
    )];
    let b = sandbox(&pkgs, "");
    installed(&b, &["net"]);
    let marker = b.home.join("latest.called");
    (b, marker)
}

#[test]
fn status_and_offline_outdated_never_ask_a_provider_for_latest() {
    let (b, marker) = marker_sandbox();
    let o = run(&b, &["status"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(!marker.exists(), "status ran `latest`");
    let o = run(&b, &["outdated", "--offline", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(!marker.exists(), "--offline ran `latest`");
    let d = json(&o);
    validate("outdated", &d);
    assert_eq!(d["data"]["offline"], true);
    assert_eq!(d["data"]["unknown"], serde_json::json!(["net"]));
    assert!(d["data"]["packages"][0]["latest"].is_null());
    let human = run(&b, &["outdated", "--offline"]);
    assert!(
        human.stdout.contains("unknown") && human.stdout.contains("net"),
        "{}",
        human.stdout
    );
}

#[test]
fn outdated_refreshes_the_cache_and_status_then_reads_it() {
    let (b, marker) = marker_sandbox();
    let o = run(&b, &["outdated"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(marker.exists());
    assert!(
        o.stdout.contains("1 package is outdated") && o.stdout.contains("1.0.0 -> 2.0.0"),
        "{}",
        o.stdout
    );
    let cache: Value =
        serde_json::from_str(&std::fs::read_to_string(b.cache().join("script.json")).unwrap())
            .unwrap();
    assert_eq!(cache["versions"]["net"], "2.0.0");

    // status now shows it, from the cache alone
    std::fs::remove_file(&marker).unwrap();
    let s = run(&b, &["status"]);
    assert_eq!(s.code, 2);
    assert!(s.stdout.contains("1.0.0 -> 2.0.0"), "{}", s.stdout);
    assert!(!marker.exists());

    // fresh cache: reused; --refresh: asked again
    let o = run(&b, &["outdated"]);
    assert!(o.stdout.contains("outdated"));
    assert!(!marker.exists(), "a fresh cache is reused");
    run(&b, &["outdated", "--refresh"]);
    assert!(marker.exists(), "--refresh forces a fetch");

    // --offline uses the cache however old it is
    std::fs::remove_file(&marker).unwrap();
    seed_cache(&b, 0, &[("net", "3.0.0")]);
    let o = run(&b, &["outdated", "--offline", "--json"]);
    assert_eq!(json(&o)["data"]["packages"][0]["latest"], "3.0.0");
    assert!(!marker.exists());

    // the 6 hour TTL: an old cache is refreshed
    let o = run(&b, &["outdated", "--json"]);
    assert!(marker.exists(), "an expired cache is refreshed");
    assert_eq!(json(&o)["data"]["packages"][0]["latest"], "2.0.0");
}

#[test]
fn outdated_can_be_limited_to_named_packages() {
    let pkgs = vec![script("a", "2.0.0", ""), script("b", "2.0.0", "")];
    let b = sandbox(&pkgs, "");
    installed(&b, &["a", "b"]);
    let d = json(&run(&b, &["outdated", "a", "--json"]));
    validate("outdated", &d);
    assert_eq!(d["data"]["packages"].as_array().unwrap().len(), 1);
    assert_eq!(d["data"]["packages"][0]["package"], "a");
    assert_eq!(run(&b, &["outdated", "nope"]).code, 1);
}

// ------------------------------------------------------------------ binaries

/// `tool` (managed, in ~/.local/bin) and `foo` (nobody's) on a PATH of `~/.local/bin`.
fn bins_sandbox() -> (Sb, String) {
    let b = sandbox(&[script("tool", "1.0.0", "bins = [\"tool\"]")], "");
    installed(&b, &["tool"]);
    exe(&b.local_bin().join("tool"));
    exe(&b.local_bin().join("foo"));
    // Only this directory: the system's own binaries would drown the listing.
    let path = b.local_bin().display().to_string();
    (b, path)
}

#[test]
#[cfg(unix)]
fn an_unowned_executable_is_an_unknown_owner() {
    let (b, path) = bins_sandbox();
    let o = run_path(&b, &path, &["bins", "--unknown"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("foo") && o.stdout.contains("unknown owner"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("~/.local/bin/foo"), "{}", o.stdout);
    assert!(!o.stdout.contains("tool "), "{}", o.stdout);
    let d = json(&run_path(&b, &path, &["bins", "--unknown", "--json"]));
    validate("bins", &d);
    let foo = d["data"]["bins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["bin"] == "foo")
        .unwrap();
    assert!(foo["package"].is_null());
    assert_eq!(foo["class"], "unknown");
    assert_eq!(foo["notes"][0], "unknown owner");
}

#[test]
#[cfg(unix)]
fn managed_binaries_name_their_package_and_provider() {
    let (b, path) = bins_sandbox();
    let d = json(&run_path(&b, &path, &["bins", "--managed", "--json"]));
    validate("bins", &d);
    let e = &d["data"]["bins"][0];
    assert_eq!(
        (e["bin"].as_str(), e["package"].as_str()),
        (Some("tool"), Some("tool"))
    );
    assert_eq!(e["provider"], "script");
    assert_eq!(e["version"], "1.0.0");
    assert_eq!(d["data"]["summary"]["managed"], 1);
    // by package and by provider
    let by_pkg = json(&run_path(&b, &path, &["bins", "tool", "--json"]));
    assert_eq!(by_pkg["data"]["bins"].as_array().unwrap().len(), 1);
    let by_prov = json(&run_path(&b, &path, &["bins", "script", "--json"]));
    assert_eq!(by_prov["data"]["bins"].as_array().unwrap().len(), 1);
    assert_eq!(run_path(&b, &path, &["bins", "bogus"]).code, 1);
    // --managed and --unknown cannot be combined
    assert_eq!(
        run_path(&b, &path, &["bins", "--managed", "--unknown"]).code,
        64
    );
}

/// A binary in a package's own `[env] path` directory belongs to that package, whichever
/// provider put it there; a binary the package does not declare, or one outside its dirs, stays
/// unowned.
#[test]
#[cfg(unix)]
fn a_binary_in_a_packages_env_path_dir_is_owned_by_that_package() {
    let b = sandbox(
        &[script(
            "fzf",
            "1.0.0",
            "bin = \"fzf\"\n[env]\npath = [\"~/.fzf/bin\"]",
        )],
        "",
    );
    installed(&b, &["fzf"]);
    exe(&b.home.join(".fzf/bin/fzf"));
    exe(&b.home.join(".fzf/bin/stray"));
    exe(&b.local_bin().join("fzf-tmux"));
    let path = b.local_bin().display().to_string();
    let d = json(&run_path(&b, &path, &["bins", "--json"]));
    validate("bins", &d);
    let find = |name: &str| {
        d["data"]["bins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["bin"] == name)
            .unwrap_or_else(|| panic!("{name} missing: {d}"))
            .clone()
    };
    let fzf = find("fzf");
    assert_eq!(fzf["package"], "fzf", "{fzf}");
    assert_eq!(fzf["class"], "managed", "{fzf}");
    assert_eq!(fzf["provider"], "script");
    assert_eq!(fzf["path"], "~/.fzf/bin/fzf");
    assert_eq!(find("stray")["class"], "unknown", "not a declared bin");
    assert_eq!(find("fzf-tmux")["class"], "unknown", "outside the env dirs");
    let w = run_path(&b, &path, &["which", "fzf"]);
    assert!(!w.stdout.contains("unknown owner"), "{}", w.stdout);
}

#[test]
#[cfg(unix)]
fn which_lists_every_copy_in_path_order_and_marks_the_winner() {
    let (b, _) = bins_sandbox();
    let late = b.home.join("late");
    exe(&late.join("tool"));
    let path = format!("{}:{}:/usr/bin", b.local_bin().display(), late.display());
    let o = run_path(&b, &path, &["which", "tool"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let lines: Vec<&str> = o.stdout.lines().collect();
    let win = lines
        .iter()
        .position(|l| l.trim_start().starts_with("> "))
        .unwrap();
    assert!(lines[win].contains("~/.local/bin/tool"), "{}", o.stdout);
    let other = lines.iter().position(|l| l.contains("late/tool")).unwrap();
    assert!(
        other > win && lines[other].contains("shadowed"),
        "{}",
        o.stdout
    );
    let d = json(&run_path(&b, &path, &["which", "tool", "--json"]));
    validate("which", &d);
    assert_eq!(d["data"]["copies"].as_array().unwrap().len(), 2);
    assert_eq!(d["data"]["copies"][0]["wins"], true);
    // and `bins --shadowed` finds the loser
    let sh = json(&run_path(&b, &path, &["bins", "--shadowed", "--json"]));
    assert_eq!(sh["data"]["bins"].as_array().unwrap().len(), 1);
    // not on PATH at all
    let missing = run_path(&b, &path, &["which", "nothing-like-this"]);
    assert_eq!(missing.code, 1);
    assert!(
        missing.stderr.contains("bin-not-found"),
        "{}",
        missing.stderr
    );
}

#[test]
fn status_flags_a_declared_binary_that_is_not_on_path() {
    let b = sandbox(&[script("tool", "1.0.0", "bins = [\"tool\"]")], "");
    installed(&b, &["tool"]);
    // installed, but nothing in ~/.local/bin
    let o = run(&b, &["status"]);
    assert_eq!(o.code, 2, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("BINS") && o.stdout.contains("not on PATH"),
        "{}",
        o.stdout
    );
}

// ------------------------------------------------------------------ doctor

#[test]
fn doctor_reports_an_unsourced_env_file_and_passes_after_apply() {
    let b = sandbox(
        &[script(
            "tool",
            "1.0.0",
            "[env]\npath = [\"{home}/.local/bin\"]\n",
        )],
        "",
    );
    installed(&b, &["tool"]);
    let o = run(&b, &["doctor"]);
    assert_eq!(o.code, 0, "warnings do not fail: {}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("1 problem found") || o.stdout.contains("problems found"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("not sourced") && o.stdout.contains("dots apply"),
        "{}",
        o.stdout
    );
    let d = json(&run(&b, &["doctor", "--json"]));
    validate("doctor", &d);
    assert!(d["data"]["summary"]["warnings"].as_u64().unwrap() >= 1);

    // apply writes the env file and the rc inject; a shell that sourced it has the dir on PATH
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let path = dots_testkit::path_with(b.local_bin());
    let o = run_path(&b, &path, &["doctor"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("All good"), "{}", o.stdout);
}

#[test]
fn doctor_reports_dangling_links_as_errors() {
    let b = sandbox(&[("shellcfg".to_string(), FILES_PKG.to_string())], "");
    let src = b.root.join("packages/shellcfg/files/aliases");
    write(&src, "x\n");
    assert_eq!(run(&b, &["apply", "--yes", "--files-only"]).code, 0);
    std::fs::remove_file(&src).unwrap();
    let o = run(&b, &["doctor"]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("dangling link") && o.stdout.contains("~/.aliases"),
        "{}",
        o.stdout
    );
    let d = json(&run(&b, &["doctor", "--json"]));
    validate("doctor", &d);
    assert_eq!(d["ok"], false);
    assert_eq!(d["data"]["summary"]["errors"], 1);
}

#[test]
#[cfg(unix)]
fn doctor_explains_path_order_shadowing() {
    let (b, _) = bins_sandbox();
    let early = b.home.join("early");
    exe(&early.join("tool"));
    // an unrelated copy comes first, so the managed one loses
    let path = format!("{}:{}", early.display(), b.local_bin().display());
    let o = run_path(&b, &path, &["doctor"]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(
        o.stdout.contains("PATH order") && o.stdout.contains("shadowed by"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("before"),
        "how to fix the order: {}",
        o.stdout
    );
}

// ------------------------------------------------------------------ goldens

/// Output with the sandbox path replaced and the machine-specific context bar dropped.
fn golden_text(o: &Out, b: &Sb) -> String {
    let norm = |s: &str| s.replace(b.tmp.path().to_string_lossy().as_ref(), "<SANDBOX>");
    // Only `status` has a context bar (root, host, platform): machine specific, so dropped.
    let mut lines = o.stdout.lines().peekable();
    let has_bar = lines.peek().is_some_and(|l| l.starts_with(" dots status"));
    let keep: Vec<&str> = if has_bar {
        lines.skip_while(|l| !l.trim().is_empty()).skip(1).collect()
    } else {
        lines.collect()
    };
    format!(
        "exit: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        o.code,
        norm(&keep.join("\n")),
        norm(&o.stderr)
    )
}

fn golden_json(o: &Out, b: &Sb) -> String {
    let mut d = json(o);
    d["data"]["header"] = Value::Null;
    let text = serde_json::to_string_pretty(&d).unwrap();
    format!(
        "exit: {}\n{}\n",
        o.code,
        text.replace(b.tmp.path().to_string_lossy().as_ref(), "<SANDBOX>")
    )
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

#[test]
fn goldens_for_status_in_plain_and_json() {
    let g = common::golden();
    let b = attention_sandbox();
    for (name, args) in [
        ("status-summary", vec!["status"]),
        ("status-all", vec!["status", "--all"]),
        ("status-deep", vec!["status", "o1"]),
        ("outdated", vec!["outdated", "--offline"]),
    ] {
        let plain = run(&b, &[&args[..], &["--color", "never"]].concat());
        g.compare(
            &golden_path(&format!("{name}.plain.txt")),
            &golden_text(&plain, &b),
        );
        let j = run(&b, &[&args[..], &["--json"]].concat());
        g.compare(
            &golden_path(&format!("{name}.json.txt")),
            &golden_json(&j, &b),
        );
    }
}

#[test]
#[cfg(unix)]
fn goldens_for_bins_which_and_doctor() {
    let g = common::golden();
    let (b, path) = bins_sandbox();
    for (name, args) in [
        ("bins", vec!["bins"]),
        ("which", vec!["which", "tool"]),
        ("doctor", vec!["doctor"]),
    ] {
        let plain = run_path(&b, &path, &[&args[..], &["--color", "never"]].concat());
        g.compare(
            &golden_path(&format!("{name}.plain.txt")),
            &golden_text(&plain, &b),
        );
    }
}

// ------------------------------------------------------------------ benchmark

/// 100 script packages (the slowest provider: two shell commands each), a warm latest cache:
/// `dots status` finishes in under 1.5 s (median of 5 runs). The in-process 100-fake-package
/// variant lives in `status.rs`.
#[test]
fn status_over_100_packages_is_under_1_5_seconds() {
    let pkgs: Vec<(String, String)> = (0..100)
        .map(|i| {
            script(
                &format!("pkg{i:03}"),
                if i % 5 == 0 { "2.0.0" } else { "1.0.0" },
                "",
            )
        })
        .collect();
    let b = sandbox(&pkgs, "");
    let names: Vec<String> = (0..90).map(|i| format!("pkg{i:03}")).collect();
    for n in &names {
        write(&b.home.join(format!("{n}.installed")), "");
    }
    let latest: Vec<(String, &str)> = (0..100)
        .map(|i| {
            (
                format!("pkg{i:03}"),
                if i % 5 == 0 { "2.0.0" } else { "1.0.0" },
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = latest.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    seed_cache(&b, now(), &refs);
    let mut samples: Vec<Duration> = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let o = run(&b, &["status", "--json"]);
        samples.push(t.elapsed());
        assert_eq!(o.code, 2, "{}", o.stderr);
        let d = json(&o);
        assert_eq!(d["data"]["counts"]["packages"]["missing"], 10);
        assert_eq!(d["data"]["counts"]["packages"]["outdated"], 18);
    }
    samples.sort();
    eprintln!(
        "dots status, 100 script packages: median {:?} ({samples:?})",
        samples[2]
    );
    assert!(samples[2] < Duration::from_millis(1500), "{samples:?}");
}

#[test]
fn every_cli_plain_golden_is_ascii() {
    let dir = golden_path("x").parent().unwrap().to_path_buf();
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.to_string_lossy().ends_with(".plain.txt") {
            let t = std::fs::read_to_string(&p).unwrap();
            assert!(t.is_ascii(), "{} has non-ASCII text", p.display());
            n += 1;
        }
    }
    assert!(n > 0);
}
