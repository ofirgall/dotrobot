//! `dots questions` covers the settings of packages pulled in through `requires`.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn questions_include_settings_of_required_packages() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    write(
        &root.join("packages/app/package.toml"),
        "schema = 1\nname = \"app\"\nrequires = [\"mid\"]\n",
    );
    write(
        &root.join("packages/mid/package.toml"),
        "schema = 1\nname = \"mid\"\nrequires = [\"lib\"]\n",
    );
    write(
        &root.join("packages/lib/package.toml"),
        "schema = 1\nname = \"lib\"\n[settings.token]\ntype = \"string\"\nrequired = true\n",
    );
    write(
        &root.join("bundles/base.toml"),
        "schema = 1\npackages = [\"app\"]\n",
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"base\"]\n",
            root.display().to_string()
        ),
    );
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("DOTS_PROVIDERS", "none")
        .env("HOME", tmp.path().join("sentinel"))
        .current_dir(tmp.path())
        .arg("--home")
        .arg(&home)
        .arg("--machine-config")
        .arg(&machine)
        .args(["--json", "questions"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let doc: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let ids: Vec<&str> = doc["data"]["questions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["setting:lib.token"], "{out}");
}
