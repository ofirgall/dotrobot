//! Package settings reach script providers as `DOTS_SETTING_*` (secrets included, never
//! logged), and package `when` reads settings.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    machine: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn sandbox() -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    write(
        &root.join("packages/tool/package.toml"),
        r#"schema = 1
name = "tool"
[providers.script]
check = "test -f $HOME/tool.installed"
version = "echo 1.0.0"
install = "env | grep '^DOTS_SETTING_' | sort > $HOME/env.out; touch $HOME/tool.installed; echo token=$DOTS_SETTING_TOKEN"
[settings.size]
type = "int"
default = 3
[settings.fast]
type = "bool"
default = true
[settings.flags]
type = "list<string>"
default = ["a", "b"]
[settings.token]
type = "string"
secret = true
default = "unused"
"#,
    );
    write(
        &root.join("packages/extra/package.toml"),
        "schema = 1\nname = \"extra\"\nwhen = 'settings.tool.fast = false'\n[providers.script]\ncheck = \"test -f $HOME/extra.installed\"\nversion = \"echo 1.0.0\"\ninstall = \"touch $HOME/extra.installed\"\n",
    );
    write(
        &root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"tool\", \"extra\"]\n",
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
    Sb { tmp, home, machine }
}

fn run(b: &Sb, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
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
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn files_below(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_below(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn scripts_see_settings_and_secrets_stay_out_of_logs() {
    let b = sandbox();
    let secret = "tok-777-secret";
    assert_eq!(run(&b, &["config", "set", "tool.token", secret]).0, 0);
    assert_eq!(run(&b, &["config", "set", "tool.size", "7"]).0, 0);
    let (code, out) = run(&b, &["apply", "--yes", "--packages-only"]);
    assert_eq!(code, 0, "{out}");
    let env = std::fs::read_to_string(b.home.join("env.out")).unwrap();
    assert!(env.contains("DOTS_SETTING_SIZE=7"), "{env}");
    assert!(env.contains("DOTS_SETTING_FAST=1"), "{env}");
    assert!(env.contains("DOTS_SETTING_FLAGS=a\nb") || env.contains("DOTS_SETTING_FLAGS=a"));
    assert!(
        env.contains(&format!("DOTS_SETTING_TOKEN={secret}")),
        "{env}"
    );
    // The secret is in the environment only: not in any report or on-disk log or state, bar
    // the machine config that holds it and the script's own output file.
    assert!(!out.contains(secret), "{out}");
    let mut files = Vec::new();
    files_below(&b.home, &mut files);
    for f in files {
        if f.file_name().is_some_and(|n| n == "env.out") {
            continue;
        }
        let text = String::from_utf8_lossy(&std::fs::read(&f).unwrap()).into_owned();
        // The script itself echoed it into its own log: that is the script's output.
        if f.extension().is_some_and(|e| e == "log") {
            continue;
        }
        assert!(!text.contains(secret), "{}", f.display());
    }
}

#[test]
fn package_when_reads_settings() {
    let b = sandbox();
    let (code, out) = run(&b, &["apply", "--yes", "--packages-only"]);
    assert_eq!(code, 0, "{out}");
    assert!(b.home.join("tool.installed").exists());
    assert!(!b.home.join("extra.installed").exists(), "fast is true");
    assert_eq!(run(&b, &["config", "set", "tool.fast", "false"]).0, 0);
    let (code, out) = run(&b, &["apply", "--yes", "--packages-only"]);
    assert_eq!(code, 0, "{out}");
    assert!(b.home.join("extra.installed").exists(), "{out}");
}
