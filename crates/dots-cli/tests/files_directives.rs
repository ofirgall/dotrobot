//! Directives, `[[files.set]]` and fragments through the real binary, in a `--home` sandbox.

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
    std::fs::write(p, dots_testkit::bash_on_windows(text)).unwrap();
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
        .envs(dots_testkit::system_env())
        .envs(envs.iter().copied())
        .env("PATH", dots_testkit::system_path())
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

const GHOSTTY: &str = r#"
[settings.font_size]
type = "int"
default = 15

[settings.telemetry]
type = "bool"
default = false

[[files]]
mode = "render"
dst = "{config}/ghostty/config"
src = "config"

[[files]]
mode = "render"
format = "json"
dst = "{config}/ghostty/settings.json"
src = "settings.json"

[[files.set]]
path = "size"
value = "@dots(setting.font_size)@"

[[files.set]]
path = "env.DISABLE_TELEMETRY"
delete = true
when = 'not(setting.telemetry)'
"#;

fn frag(dst: &str, prio: i64) -> String {
    format!(
        "\n[[files]]\nmode = \"fragment\"\nformat = \"gitconfig\"\ndst = \"{dst}\"\nsrc = \"gitconfig.fragment\"\npriority = {prio}\n"
    )
}

#[test]
fn directives_and_set_patches_render_with_package_settings() {
    let b = sandbox(&[("ghostty", GHOSTTY)]);
    let p = b.root.join("packages/ghostty");
    write(
        &p.join("config"),
        "font-size = @dots(setting.font_size)@\n# dots:if os = \"macos\"\nmac = 1\n# dots:else\nother = 1\n# dots:end\n",
    );
    write(
        &p.join("settings.json"),
        r#"{"env": {"DISABLE_TELEMETRY": "1", "KEEP": "1"}}"#,
    );
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let cfg = std::fs::read_to_string(b.home.join(".config/ghostty/config")).unwrap();
    assert!(cfg.contains("font-size = 15\n"), "{cfg}");
    assert!(cfg.contains("font_size=15(default)"), "{cfg}");
    assert_eq!(cfg.contains("mac = 1"), cfg!(target_os = "macos"), "{cfg}");
    let s: Value = serde_json::from_str(
        &std::fs::read_to_string(b.home.join(".config/ghostty/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(s, serde_json::json!({"env": {"KEEP": "1"}, "size": 15}));
    // Converged: nothing left to do, and status agrees.
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);
}

#[test]
fn an_unknown_name_fails_that_file_with_file_and_line() {
    let b = sandbox(&[("ghostty", GHOSTTY)]);
    let p = b.root.join("packages/ghostty");
    write(&p.join("config"), "a = 1\nb = @dots(setting.nope)@\n");
    write(&p.join("settings.json"), "{}");
    let o = run(&b, &["plan", "--json"]);
    let text = format!("{}{}", o.stdout, o.stderr);
    assert!(text.contains("unknown name `setting.nope`"), "{text}");
    assert!(text.contains("config:2"), "{text}");
}

#[test]
fn fragments_from_two_packages_compose_one_file_and_removing_one_drops_its_part() {
    let b = sandbox(&[
        ("base", &frag("{home}/.gitconfig", 0)),
        ("delta", &frag("{home}/.gitconfig", 50)),
    ]);
    write(
        &b.root.join("packages/base/gitconfig.fragment"),
        "[core]\n\teditor = nvim\n",
    );
    write(
        &b.root.join("packages/delta/gitconfig.fragment"),
        "[delta]\n\tnavigate = true\n",
    );
    let o = run(&b, &["plan", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let d = json(&o);
    let files = d["data"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "{d}");
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let g = std::fs::read_to_string(b.home.join(".gitconfig")).unwrap();
    assert!(g.contains("[core]") && g.contains("[delta]"), "{g}");
    assert!(g.contains("# fragments: base (priority 0"), "{g}");
    assert_eq!(run(&b, &["plan", "--check"]).code, 0);

    // A targeted apply of one contributor keeps the other's part.
    let o = run(&b, &["apply", "--yes", "base"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let g = std::fs::read_to_string(b.home.join(".gitconfig")).unwrap();
    assert!(g.contains("[delta]"), "{g}");

    // Drop `delta` from the machine: the same file is re-rendered without [delta].
    write(
        &b.root.join("bundles/all.toml"),
        "schema = 1\npackages = [\"base\"]\n",
    );
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let g = std::fs::read_to_string(b.home.join(".gitconfig")).unwrap();
    assert!(g.contains("[core]") && !g.contains("[delta]"), "{g}");

    // And with no contributor left the file goes.
    write(
        &b.root.join("bundles/all.toml"),
        "schema = 1\npackages = []\n",
    );
    let o = run(&b, &["apply", "--yes", "--json"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert!(!b.home.join(".gitconfig").exists());
}

#[test]
fn a_fragment_collision_fails_the_file_and_lint_reports_it() {
    let b = sandbox(&[
        ("base", &frag("{home}/.gitconfig", 0)),
        ("delta", &frag("{home}/.gitconfig", 0)),
    ]);
    write(
        &b.root.join("packages/base/gitconfig.fragment"),
        "[core]\n\teditor = vi\n",
    );
    write(
        &b.root.join("packages/delta/gitconfig.fragment"),
        "[core]\n\teditor = nvim\n",
    );
    let o = run(&b, &["lint", "--json"]);
    let text = format!("{}{}", o.stdout, o.stderr);
    assert!(text.contains("fragment-collision"), "{text}");
    let o = run(&b, &["plan", "--json"]);
    let text = format!("{}{}", o.stdout, o.stderr);
    assert!(text.contains("both set `core.editor`"), "{text}");
    assert!(!b.home.join(".gitconfig").exists());
}
