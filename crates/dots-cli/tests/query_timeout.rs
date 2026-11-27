//! A `check` or `version` that hangs while planning fails fast and names the command, instead
//! of holding the whole plan (bug 38).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A repo with `hung` (given check and version) and `fine`, planned with a 1 second query
/// limit. Returns (exit code, output, seconds it took).
fn plan(hung_check: &str, hung_version: &str, args: &[&str]) -> (i32, String, Duration) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    write(
        &root.join("packages/hung/package.toml"),
        &format!(
            "schema = 1\nname = \"hung\"\n[providers.script]\ncheck = {hung_check:?}\nversion = {hung_version:?}\ninstall = \"true\"\n"
        ),
    );
    write(
        &root.join("packages/fine/package.toml"),
        "schema = 1\nname = \"fine\"\n[providers.script]\ncheck = \"false\"\nversion = \"echo 1.0.0\"\ninstall = \"true\"\n",
    );
    write(
        &root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"hung\", \"fine\"]\n",
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
    let start = Instant::now();
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("DOTS_PROVIDERS", "script")
        .env("DOTS_QUERY_TIMEOUT", "1s")
        .env("HOME", tmp.path().join("sentinel"))
        .current_dir(tmp.path())
        .arg("--home")
        .arg(&home)
        .arg("--machine-config")
        .arg(&machine)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        start.elapsed(),
    )
}

#[test]
fn a_hung_check_is_an_error_for_its_package_and_the_plan_goes_on() {
    let (code, out, took) = plan("sleep 60", "echo 1", &["plan"]);
    assert!(took < Duration::from_secs(30), "took {took:?}");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("query-timeout"), "{out}");
    assert!(out.contains("`hung`") && out.contains("sleep 60"), "{out}");
    // The other package is still planned.
    assert!(out.contains("fine"), "{out}");
}

#[test]
fn a_hung_check_stops_apply_before_it_changes_anything() {
    let (code, out, took) = plan("sleep 60", "echo 1", &["apply", "--yes"]);
    assert!(took < Duration::from_secs(30), "took {took:?}");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("query-timeout"), "{out}");
}

#[test]
fn a_hung_version_is_a_warning_and_the_plan_succeeds() {
    let (code, out, took) = plan("true", "sleep 60", &["plan"]);
    assert!(took < Duration::from_secs(30), "took {took:?}");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("version-query-timeout"), "{out}");
    assert!(out.contains("`hung`"), "{out}");
}
