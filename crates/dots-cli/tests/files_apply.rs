//! The files phase and `[env]` through the real binary, in a `--home` sandbox.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
    machine: PathBuf,
}

impl Sb {
    fn state(&self) -> PathBuf {
        self.home.join(".local/state/dots")
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// `pkgs`: (name, package.toml body after the header). Every package is in bundle `all`.
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
    run_env(b, args, &[])
}

fn run_env(b: &Sb, args: &[&str], envs: &[(&str, &Path)]) -> Out {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(envs.iter().copied())
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/zsh")
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
        code: o.status.code().unwrap_or(-1),
    }
}

fn json(o: &Out) -> Value {
    serde_json::from_str(&o.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", o.stdout, o.stderr))
}

const FILES_PKG: &str = r#"
[[files]]
src = "files/aliases"
dst = "{home}/.aliases"

[[files]]
mode = "render"
dst = "{config}/app/conf.toml"
layers = ["layers/common.toml", "layers/extra.toml"]
"#;

fn files_sandbox() -> Sb {
    let b = sandbox(&[("shellcfg", FILES_PKG)]);
    let p = b.root.join("packages/shellcfg");
    write(&p.join("files/aliases"), "alias ll='ls -l'\n");
    write(
        &p.join("layers/common.toml"),
        "a = 1\n[s]\nx = \"common\"\n",
    );
    write(&p.join("layers/extra.toml"), "b = 2\n[s]\ny = \"extra\"\n");
    b
}

#[test]
fn plan_lists_files_and_apply_creates_them_through_active() {
    let b = files_sandbox();
    let o = run(&b, &["plan", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let d = json(&o);
    let files = d["data"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{d}");
    assert!(files.iter().all(|f| f["action"] == "create"));
    assert!(files.iter().any(|f| f["kind"] == "link"));
    assert!(files.iter().any(|f| f["kind"] == "render"));
    assert_eq!(run(&b, &["plan", "--check"]).code, 3);
    let human = run(&b, &["plan"]);
    assert!(human.stdout.contains("~/.aliases"), "{}", human.stdout);
    assert!(human.stdout.contains("render"), "{}", human.stdout);
    assert!(!human.stdout.contains("Not planned"), "{}", human.stdout);

    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let d = json(&o);
    assert_eq!(d["data"]["summary"]["files_ok"], 2);

    let alias = b.home.join(".aliases");
    let target = std::fs::read_link(&alias).unwrap();
    assert!(target.starts_with(b.state().join("active")), "{target:?}");
    assert_eq!(
        std::fs::read_to_string(&alias).unwrap(),
        "alias ll='ls -l'\n"
    );
    let conf = std::fs::read_to_string(b.home.join(".config/app/conf.toml")).unwrap();
    assert!(
        conf.contains("x = \"common\"") && conf.contains("y = \"extra\""),
        "{conf}"
    );
    assert!(conf.contains("b = 2"), "{conf}");

    // converged: nothing to do, check is clean
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let d = json(&o);
    assert_eq!(d["data"]["plan"]["empty"], true, "{d}");
    assert_eq!(d["data"]["files"].as_array().unwrap().len(), 0);
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);
}

#[test]
fn files_only_and_packages_only_scope_the_phases() {
    let b = sandbox(&[(
        "tool",
        r#"
[providers.script]
check = "test -f $HOME/tool.installed"
version = "echo 1.0.0"
install = "touch $HOME/tool.installed"

[[files]]
src = "files/rc"
dst = "{home}/.toolrc"
"#,
    )]);
    write(&b.root.join("packages/tool/files/rc"), "x\n");
    let o = run(&b, &["apply", "--files-only", "--yes"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(b.home.join(".toolrc").exists());
    assert!(!b.home.join("tool.installed").exists());
    let o = run(&b, &["apply", "--packages-only", "--yes"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(b.home.join("tool.installed").exists());
}

#[test]
fn a_bad_render_fails_only_that_file() {
    let b = sandbox(&[(
        "cfg",
        r#"
[[files]]
mode = "render"
dst = "{config}/bad.toml"
layers = ["layers/bad.toml"]

[[files]]
src = "files/ok"
dst = "{home}/.ok"
"#,
    )]);
    let p = b.root.join("packages/cfg");
    write(&p.join("layers/bad.toml"), "this is = = not toml\n");
    write(&p.join("files/ok"), "ok\n");
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 1, "{}", o.stderr);
    let d = json(&o);
    assert_eq!(d["data"]["summary"]["files_failed"], 1, "{d}");
    assert_eq!(d["data"]["summary"]["files_ok"], 1);
    assert!(b.home.join(".ok").exists());
    assert!(!b.home.join(".config/bad.toml").exists());
}

#[test]
fn an_existing_user_file_is_backed_up_and_restore_puts_it_back() {
    let b = files_sandbox();
    write(&b.home.join(".aliases"), "mine\n");
    let o = run(&b, &["plan", "--json"]);
    let d = json(&o);
    let f = d["data"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "link")
        .unwrap();
    assert_eq!(f["action"], "replace", "{d}");

    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let run_id = json(&o)["data"]["run"].as_str().unwrap().to_string();
    assert!(
        std::fs::symlink_metadata(b.home.join(".aliases"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let o = run(&b, &["restore", &run_id]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        std::fs::read_to_string(b.home.join(".aliases")).unwrap(),
        "mine\n"
    );
}

fn env_sandbox() -> Sb {
    sandbox(&[
        (
            "a",
            r#"
[providers.script]
check = "test -x $HOME/bin-a/toola"
version = "echo 1.0.0"
install = "mkdir -p $HOME/bin-a && printf '#!/bin/sh\necho hi\n' > $HOME/bin-a/toola && chmod +x $HOME/bin-a/toola"

[env]
path = ["~/bin-a"]
vars = { A_HOME = "~/bin-a" }
"#,
        ),
        (
            "b",
            r#"
requires = ["a"]

[providers.script]
check = "test -f $HOME/b.installed"
version = "echo 1.0.0"
install = "toola > $HOME/b.installed"
"#,
        ),
    ])
}

#[test]
fn a_fresh_tool_is_on_path_for_later_packages_in_the_same_run() {
    let b = env_sandbox();
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert_eq!(
        std::fs::read_to_string(b.home.join("b.installed")).unwrap(),
        "hi\n"
    );
    let sh = std::fs::read_to_string(b.state().join("env.sh")).unwrap();
    assert!(sh.contains("bin-a") && sh.contains("A_HOME"), "{sh}");
    assert!(b.state().join("env.fish").exists() && b.state().join("env.ps1").exists());
    let rc = std::fs::read_to_string(b.home.join(".zshenv")).unwrap();
    assert_eq!(rc.matches("dots env").count(), 2, "{rc}");
}

#[test]
fn a_second_apply_leaves_env_files_and_rc_unchanged() {
    let b = env_sandbox();
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
    let before = (
        read(b.state().join("env.sh")),
        read(b.state().join("env.fish")),
        read(b.state().join("env.ps1")),
        read(b.home.join(".zshenv")),
    );
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(json(&o)["data"]["plan"]["empty"], true);
    let after = (
        read(b.state().join("env.sh")),
        read(b.state().join("env.fish")),
        read(b.state().join("env.ps1")),
        read(b.home.join(".zshenv")),
    );
    assert_eq!(before, after);
}

#[cfg(unix)]
#[test]
fn override_and_machine_layers_render_on_top_of_package_layers() {
    let b = sandbox(&[(
        "app",
        "[[files]]\nmode = \"render\"\ndst = \"{config}/app/conf.toml\"\nlayers = [\"layers/common.toml\"]\n",
    )]);
    write(
        &b.root.join("packages/app/layers/common.toml"),
        "base = 1\nwho = \"package\"\n[s]\nkeep = true\n",
    );
    write(
        &b.root.join("overrides/packages/app/conf.toml"),
        "who = \"repo\"\nrepo_only = 1\n",
    );
    write(&b.root.join("machines/work.toml"), "schema = 1\n");
    write(
        &b.root
            .join("machines/work/overrides/packages/app/conf.toml"),
        "who = \"profile\"\nprofile_only = 2\n",
    );
    let mut m = std::fs::read_to_string(&b.machine).unwrap();
    m.push_str("profile = \"work\"\n");
    write(&b.machine, &m);
    write(
        &b.home.join(".config/dots/overrides/packages/app/conf.toml"),
        "[s]\nlocal = \"yes\"\n",
    );

    // The repo is not at `~/dotfiles`: the profile is found against the root machine.toml names.
    assert!(!b.home.join("dotfiles").exists());
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let text = std::fs::read_to_string(b.home.join(".config/app/conf.toml")).unwrap();
    for want in [
        "base = 1",
        "who = \"profile\"",
        "repo_only = 1",
        "profile_only = 2",
        "keep = true",
        "local = \"yes\"",
    ] {
        assert!(text.contains(want), "missing `{want}` in:\n{text}");
    }
    assert!(
        !text.contains("who = \"package\"") && !text.contains("who = \"repo\""),
        "{text}"
    );
}

#[test]
fn removing_the_last_env_contribution_removes_the_rc_block_and_empties_env_files() {
    let b = env_sandbox();
    write(&b.home.join(".zshenv"), "export KEEP=1\n");
    assert_eq!(run(&b, &["apply", "--yes"]).code, 0);
    let rc = std::fs::read_to_string(b.home.join(".zshenv")).unwrap();
    assert!(
        rc.starts_with("export KEEP=1\n") && rc.contains("dots env"),
        "{rc}"
    );

    let a = b.root.join("packages/a/package.toml");
    let text = std::fs::read_to_string(&a).unwrap();
    write(&a, text.split("[env]").next().unwrap());
    assert_eq!(run(&b, &["plan", "--check"]).code, 3);
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let rc = std::fs::read_to_string(b.home.join(".zshenv")).unwrap();
    assert_eq!(rc, "export KEEP=1\n");
    let sh = std::fs::read_to_string(b.state().join("env.sh")).unwrap();
    assert!(!sh.contains("A_HOME") && !sh.contains("bin-a"), "{sh}");
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);
}

/// `status` and `apply` share one files resolution, so right after `apply` every file is `ok`,
/// override layers included.
#[test]
fn status_right_after_apply_shows_every_file_ok() {
    let b = files_sandbox();
    write(
        &b.root.join("overrides/packages/shellcfg/conf.toml"),
        "c = 3\n",
    );
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let o = run(&b, &["status", "--all", "--json"]);
    let d = json(&o);
    let rows = d["data"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{d}"));
    let files: Vec<_> = rows.iter().filter(|r| r["area"] == "files").collect();
    assert_eq!(files.len(), 2, "{d}");
    assert!(files.iter().all(|r| r["state"] == "ok"), "{files:?}");
}

/// Root precedence: `DOTS_ROOT` beats the `root` in machine.toml.
#[test]
fn dots_root_env_beats_machine_toml_root() {
    let b = files_sandbox();
    let real = std::fs::read_to_string(&b.machine).unwrap();
    write(
        &b.machine,
        &real.replace(&b.root.display().to_string(), "/nonexistent/repo"),
    );
    assert_ne!(run(&b, &["plan", "--check"]).code, 3);
    let o = run_env(&b, &["plan", "--check"], &[("DOTS_ROOT", &b.root)]);
    assert_eq!(o.code, 3, "{} {}", o.stdout, o.stderr);
}

/// No repo at the root: status, plan and apply say the machine is not initialized and point at
/// `dots init`, rather than failing on a missing file.
#[test]
fn commands_without_a_repo_report_not_initialized() {
    let b = files_sandbox();
    std::fs::remove_dir_all(&b.root).unwrap();
    for args in [&["status"][..], &["plan"], &["apply", "--yes"]] {
        let mut a = args.to_vec();
        a.push("--json");
        let o = run(&b, &a);
        assert_eq!(o.code, 1, "{args:?} {} {}", o.stdout, o.stderr);
        let d = json(&o);
        assert_eq!(d["errors"][0]["code"], "not-initialized", "{args:?} {d}");
        assert!(
            d["errors"][0]["hint"]
                .as_str()
                .unwrap()
                .contains("dots init"),
            "{d}"
        );
    }
    let o = run(&b, &["status"]);
    assert!(
        o.stderr.contains("not-initialized") && o.stderr.contains("dots init"),
        "{}",
        o.stderr
    );
}
