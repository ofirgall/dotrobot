//! Review regressions: secret values in validation messages, machine.toml permissions, a
//! `fetch` without `url`, directive comment leaders.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Sb {
    tmp: tempfile::TempDir,
    home: PathBuf,
    machine: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
}

fn sandbox(packages: &[(&str, &str)], machine_extra: &str) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for (name, text) in packages {
        write(&root.join(format!("packages/{name}/package.toml")), text);
        names.push(format!("{name:?}"));
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
    Sb { tmp, home, machine }
}

/// (exit code, stdout + stderr)
fn run(b: &Sb, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "script")
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
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

const SECRET: &str = "real-secret-value-9f3a";

const TOKEN_PKG: &str = r#"schema = 1
name = "gh"
[providers.script]
check = "true"
version = "echo 1.0.0"
install = "true"
[settings.token]
type = "string"
required = true
secret = true
validate = '^ghp_'
[settings.tokens]
type = "list<string>"
default = ["ghp_a"]
secret = true
validate = '^ghp_'
[settings.size]
type = "int"
required = true
secret = true
[settings.plain]
type = "int"
default = 1
"#;

#[test]
fn a_bad_secret_never_appears_in_any_output() {
    let b = sandbox(
        &[("gh", TOKEN_PKG)],
        &format!(
            "[settings.gh]\ntoken = \"{SECRET}\"\ntokens = [\"ghp_ok\", \"{SECRET}\"]\nsize = \"{SECRET}\"\n"
        ),
    );
    let cmds: &[&[&str]] = &[
        &["config", "explain", "gh.token"],
        &["--json", "config", "explain", "gh.token"],
        &["config", "explain", "gh.tokens"],
        &["--json", "config", "explain", "gh.tokens"],
        &["config", "explain", "gh.size"],
        &["--json", "config", "explain", "gh.size"],
        &["config", "list"],
        &["--json", "config", "list"],
        &["lint"],
        &["--json", "lint"],
        &["plan"],
        &["--json", "plan"],
        &["questions"],
        &["--json", "questions"],
        &["status"],
        &["--json", "status"],
    ];
    let mut explained = false;
    for args in cmds {
        let (_, out) = run(&b, args);
        assert!(!out.contains(SECRET), "{args:?} leaked the secret:\n{out}");
        explained |= out.contains("does not match");
    }
    assert!(explained, "the messages should still say what was wrong");
    // Setting a bad secret on the command line does not echo it either.
    let (code, out) = run(&b, &["config", "set", "gh.token", SECRET]);
    assert_ne!(code, 0);
    assert!(!out.contains(SECRET), "{out}");
    let (_, out) = run(&b, &["config", "set", "gh.size", SECRET]);
    assert!(!out.contains(SECRET), "{out}");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn config_set_keeps_machine_toml_mode_and_secrets_make_it_private() {
        let b = sandbox(&[("gh", TOKEN_PKG)], "");
        std::fs::set_permissions(&b.machine, std::fs::Permissions::from_mode(0o640)).unwrap();
        let (code, out) = run(&b, &["config", "set", "gh.plain", "3"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(mode(&b.machine), 0o640, "a plain set keeps the mode");
        let (code, out) = run(&b, &["config", "set", "gh.token", "ghp_x"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(mode(&b.machine), 0o600, "a secret makes it private");
        let (code, out) = run(&b, &["config", "unset", "gh.plain"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(mode(&b.machine), 0o600, "unset keeps it private");
    }

    #[test]
    fn config_set_writes_through_a_symlinked_machine_toml() {
        let b = sandbox(&[("gh", TOKEN_PKG)], "");
        let real = b.tmp.path().join("dotfiles-private/machine.real.toml");
        write(&real, &std::fs::read_to_string(&b.machine).unwrap());
        std::fs::remove_file(&b.machine).unwrap();
        std::os::unix::fs::symlink(&real, &b.machine).unwrap();
        let (code, out) = run(&b, &["config", "set", "gh.token", "ghp_x"]);
        assert_eq!(code, 0, "{out}");
        assert!(
            std::fs::symlink_metadata(&b.machine)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must stay a link"
        );
        assert!(std::fs::read_to_string(&real).unwrap().contains("ghp_x"));
        assert_eq!(mode(&real), 0o600);
    }
}

#[test]
fn a_fetch_without_url_fails_the_package_in_plan_and_apply() {
    let pkg = r#"schema = 1
name = "tool"
[providers.script]
check = "test -f $HOME/tool.installed"
version = "echo 1.0.0"
install = "touch $HOME/tool.installed"
[providers.script.fetch]
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#;
    let b = sandbox(&[("tool", pkg)], "");
    for cmd in [&["plan"][..], &["apply", "--yes"][..]] {
        let (code, out) = run(&b, cmd);
        assert_ne!(code, 0, "{cmd:?} must fail: {out}");
        assert!(out.contains("package.toml:"), "{cmd:?}: {out}");
        assert!(out.contains("needs a `url`"), "{cmd:?}: {out}");
    }
    assert!(!b.home.join("tool.installed").exists());
}

#[test]
fn lint_flags_directives_behind_the_wrong_comment_leader() {
    let pkg = r#"schema = 1
name = "nvim"
[[files]]
mode = "render"
src = "files/init.lua"
dst = "{home}/.config/nvim/init.lua"
[[files]]
mode = "render"
src = "files/odd.conf"
dst = "{home}/.config/odd.conf"
"#;
    let b = sandbox(&[("nvim", pkg)], "");
    let root = b.tmp.path().join("repo/packages/nvim/files");
    // The directive must hold on the machine running the test.
    let host = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    write(
        &root.join("init.lua"),
        &format!("-- dots:if os = \"{host}\"\nx = 1\n-- dots:end\n"),
    );
    write(
        &root.join("odd.conf"),
        "-- dots:if os = \"macos\"\nx = 1\n-- dots:end\n",
    );
    let (code, out) = run(&b, &["lint"]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("odd.conf"), "{out}");
    assert!(out.contains("comment = "), "{out}");
    assert!(!out.contains("init.lua"), "lua uses `--` already: {out}");
    // Plan fails the same way instead of quietly copying the directives.
    let (_, out) = run(&b, &["plan"]);
    assert!(out.contains("failed") && out.contains("odd.conf"), "{out}");
    // Apply renders init.lua with the lua leader in its header.
    std::fs::remove_file(root.join("odd.conf")).unwrap();
    write(&root.join("odd.conf"), "plain\n");
    let (code, out) = run(&b, &["apply", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let rendered = std::fs::read_to_string(b.home.join(".config/nvim/init.lua")).unwrap();
    assert!(rendered.starts_with("-- "), "{rendered}");
    assert!(
        rendered.contains("x = 1") && !rendered.contains("dots:if"),
        "{rendered}"
    );
}

#[test]
fn config_list_includes_settings_of_required_packages() {
    let app = "schema = 1\nname = \"app\"\nrequires = [\"lib\"]\n";
    let lib = "schema = 1\nname = \"lib\"\n[settings.level]\ntype = \"int\"\ndefault = 4\n";
    let b = sandbox(&[("app", app)], "");
    write(&b.tmp.path().join("repo/packages/lib/package.toml"), lib);
    let (code, out) = run(&b, &["config", "list"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("lib.level"), "{out}");
}
