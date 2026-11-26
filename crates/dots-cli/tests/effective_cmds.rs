//! `dots facts` and `dots init` read the effective model (overrides applied), like every
//! other command.

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

fn sandbox(machine_toml: Option<&str>) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let machine = tmp.path().join("machine.toml");
    if let Some(m) = machine_toml {
        write(&machine, m);
    }
    Sb {
        tmp,
        home,
        root,
        machine,
    }
}

fn run(b: &Sb, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "none")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn facts_chain_uses_platforms_declared_by_an_override() {
    let b = sandbox(None);
    write(
        &b.machine,
        &format!(
            "schema = 1\nroot = \"{}\"\nplatform_extra = [\"pop\"]\n",
            b.root.display()
        ),
    );
    write(
        &b.root.join("overrides/dots.toml"),
        "schema = 1\n[platforms.pop]\nparent = \"ubuntu\"\n",
    );
    let (code, out, err) = run(&b, &["facts", "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let doc: Value = serde_json::from_str(&out).unwrap();
    let chain: Vec<&str> = doc["data"]["platform_chain"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let (u, p) = (
        chain.iter().position(|c| *c == "ubuntu"),
        chain.iter().position(|c| *c == "pop"),
    );
    assert!(u.is_some() && u < p, "{chain:?}");
}

#[test]
fn init_answers_come_from_the_effective_model() {
    // A bundle override adds a recommended flag; a package override changes a setting default.
    let b = sandbox(None);
    write(
        &b.root.join("bundles/base.toml"),
        "schema = 1\npackages = [\"tool\"]\n",
    );
    write(
        &b.root.join("packages/tool/package.toml"),
        "schema = 1\nname = \"tool\"\n[settings.mode]\ntype = \"string\"\ndefault = \"plain\"\n",
    );
    write(
        &b.root.join("overrides/packages/tool/override.toml"),
        "schema = 1\n[settings.mode]\ndefault = \"fancy\"\n",
    );
    write(
        &b.root.join("overrides/bundles/base.toml"),
        "schema = 1\nrecommended = true\n",
    );
    let (code, out, err) = run(
        &b,
        &[
            "init",
            "--defaults",
            "--json",
            "--root",
            &b.root.display().to_string(),
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let doc: Value = serde_json::from_str(&out).unwrap();
    // `recommended` exists only in the override: defaults pick the recommended bundle.
    assert_eq!(doc["data"]["bundles"], serde_json::json!(["base"]), "{out}");
}
