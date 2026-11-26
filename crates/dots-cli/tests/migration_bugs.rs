//! Regressions for bugs found migrating a real dotfiles repo: package hooks run, provider
//! shims are created and checked, a directory without `package.toml` is a lint warning.

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

fn sandbox(pkgs: &[(&str, &str)]) -> Sb {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for (n, body) in pkgs {
        write(
            &root.join(format!("packages/{n}/package.toml")),
            &format!("schema = 1\nname = \"{n}\"\n{body}\n"),
        );
        names.push(format!("\"{n}\""));
    }
    write(&root.join("packages/x/files/rc"), "rc\n");
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
    Sb { tmp, home, machine }
}

fn run(b: &Sb, args: &[&str]) -> (i32, String) {
    let path = format!("{}/pathbin:/usr/bin:/bin", b.home.display());
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", path)
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

const HOOKED: &str = r#"
[[files]]
src = "files/rc"
dst = "~/.xrc"
[hooks]
post_files = "echo \"$DOTS_PKG $DOTS_HOOK $HOME\" > $HOME/hook.out"
"#;

#[test]
fn a_post_files_hook_runs_with_the_script_environment_and_a_dry_run_does_not() {
    let b = sandbox(&[("x", HOOKED)]);
    let out = b.home.join("hook.out");
    let (code, text) = run(&b, &["plan"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("post_files"), "plan lists the hook: {text}");
    let (code, text) = run(&b, &["apply", "--dry-run"]);
    assert_eq!(code, 0, "{text}");
    assert!(!out.exists(), "--dry-run must not run hooks");
    let (code, text) = run(&b, &["apply", "--yes"]);
    assert_eq!(code, 0, "{text}");
    let got = std::fs::read_to_string(&out).expect("the hook ran");
    assert_eq!(
        got.trim(),
        format!("x post_files {}", b.home.display()),
        "DOTS_* variables and the sandboxed HOME reach the hook"
    );
    // Files unchanged now: the hook does not fire again.
    std::fs::remove_file(&out).unwrap();
    let (code, text) = run(&b, &["apply", "--yes"]);
    assert_eq!(code, 0, "{text}");
    assert!(!out.exists(), "{text}");
    // A failing hook is reported and fails the run.
    let b = sandbox(&[("x", &HOOKED.replace("echo ", "exit 4; echo "))]);
    let (code, text) = run(&b, &["apply", "--yes"]);
    assert_ne!(code, 0, "{text}");
    assert!(text.contains("hook"), "{text}");
}

const SHIMMED: &str = r#"
bin = "fdfind"
[providers.script]
bin = "fdfind"
shim = "fd"
check = "test -x $HOME/pathbin/fdfind"
versionless = true
install = "mkdir -p $HOME/pathbin; printf '#!/bin/sh\\n' > $HOME/pathbin/fdfind; chmod +x $HOME/pathbin/fdfind"
"#;

#[test]
fn a_provider_shim_is_created_and_status_notices_when_it_goes_missing() {
    let b = sandbox(&[("fd", SHIMMED)]);
    let link = b.home.join(".local/bin/fd");
    let (code, text) = run(&b, &["apply", "--yes", "--packages-only"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        b.home.join("pathbin/fdfind")
    );
    let (_, text) = run(&b, &["status"]);
    assert!(!text.contains("shim"), "{text}");
    std::fs::remove_file(&link).unwrap();
    let (_, text) = run(&b, &["status", "--all"]);
    assert!(text.contains("shim ~/.local/bin/fd is missing"), "{text}");
    // Apply puts it back.
    let (code, text) = run(&b, &["apply", "--yes", "--packages-only"]);
    assert_eq!(code, 0, "{text}");
    assert!(std::fs::symlink_metadata(&link).is_ok());
}

#[test]
fn a_directory_without_package_toml_only_warns() {
    let b = sandbox(&[("x", HOOKED)]);
    std::fs::create_dir_all(b.tmp.path().join("repo/packages/placeholder")).unwrap();
    let (code, text) = run(&b, &["lint"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("package-missing-manifest") || text.contains("packages/placeholder"),
        "{text}"
    );
    let (code, text) = run(&b, &["plan"]);
    assert_eq!(code, 0, "{text}");
}

// ------------------------------------------------------------------ items 10, 15, 16

/// `dots.toml` extras and the repo's packages, with absolute paths of the sandbox available.
fn sandbox_with(extra: &str, pkgs: &[(&str, &str)]) -> Sb {
    let b = sandbox(pkgs);
    let toml = b.tmp.path().join("repo/dots.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        format!(
            "{text}{}",
            extra.replace("@TMP@", &b.tmp.path().display().to_string())
        ),
    )
    .unwrap();
    b
}

fn run_with(b: &Sb, providers: &str, args: &[&str]) -> (i32, String) {
    let path = format!("{}/pathbin:/usr/bin:/bin", b.home.display());
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .env("PATH", path)
        .env("DOTS_PROVIDERS", providers)
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

/// A declarative provider (`[providers.mytool]` in dots.toml) whose tool is installed by
/// another package of the same run is "will be available": it is planned after that package
/// (via `bootstrap`), not rejected with `no-provider-satisfies`, and apply installs both.
#[test]
fn a_declarative_provider_whose_tool_comes_from_a_package_in_the_run_is_planned_after_it() {
    let list = "@TMP@/mytool.list";
    let b = sandbox_with(
        &format!(
            r#"
[providers.mytool]
available = "command -v mytool"
bootstrap = "mytool-cli"
installed = "cat {list} 2>/dev/null || true"
install = "mytool add {{name}}"
batch = false
"#
        ),
        &[
            (
                "mytool-cli",
                r#"[providers.script]
check = "test -x $HOME/pathbin/mytool"
versionless = true
install = "mkdir -p $HOME/pathbin; printf '#!/bin/sh\necho \"$2\" >> @LIST@\n' > $HOME/pathbin/mytool; chmod +x $HOME/pathbin/mytool"
"#,
            ),
            ("thing", "[providers.mytool]"),
        ],
    );
    // (the script above needs the sandbox path: patch the placeholder in the package file)
    let pkg = b.tmp.path().join("repo/packages/mytool-cli/package.toml");
    let t = std::fs::read_to_string(&pkg).unwrap();
    std::fs::write(
        &pkg,
        t.replace("@LIST@", &format!("{}/mytool.list", b.tmp.path().display())),
    )
    .unwrap();
    assert!(!b.home.join("pathbin/mytool").exists());
    let (code, text) = run_with(&b, "script,mytool", &["plan"]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("no-provider-satisfies"), "{text}");
    assert!(!text.contains("no provider"), "{text}");
    let (a, t) = (text.find("mytool-cli"), text.find("thing"));
    assert!(
        a.is_some() && t.is_some() && a < t,
        "bootstrap package first: {text}"
    );
    let (code, text) = run_with(&b, "script,mytool", &["apply", "--yes", "--packages-only"]);
    assert_eq!(code, 0, "{text}");
    let list = std::fs::read_to_string(b.tmp.path().join("mytool.list")).unwrap();
    assert_eq!(list.trim(), "thing", "installed through the new provider");
}

/// Script `check`/`version` (and so `status`/`plan`) see the `[env] path` directories of the
/// active packages: a tool in `~/.tool-bin` is installed, not missing.
#[test]
fn script_commands_see_the_env_path_directories_of_active_packages() {
    let b = sandbox(&[
        (
            "toolbin",
            "[env]\npath = [\"~/.tool-bin\"]\n[providers.script]\ncheck = \"true\"\nversionless = true\ninstall = \"true\"",
        ),
        (
            "mytool",
            "requires = [\"toolbin\"]\n[providers.script]\ncheck = \"command -v mytool\"\nversion = \"mytool --version\"\ninstall = \"echo should-not-run; exit 1\"",
        ),
    ]);
    let bin = b.home.join(".tool-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("mytool");
    std::fs::write(&exe, "#!/bin/sh\necho 3.2.1\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (code, text) = run(&b, &["status", "--json"]);
    assert_eq!(code, 0, "{text}");
    let doc: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    let counts = &doc["data"]["counts"]["packages"];
    assert_eq!(counts["ok"], 2, "mytool is found via ~/.tool-bin: {text}");
    assert!(counts.get("missing").is_none(), "{text}");
    let (code, text) = run(&b, &["plan", "--check", "--packages-only"]);
    assert_eq!(code, 0, "nothing to do: {text}");
    // Control: without the declared env path the tool really is not on PATH.
    let pkg = b.tmp.path().join("repo/packages/toolbin/package.toml");
    let t = std::fs::read_to_string(&pkg).unwrap();
    std::fs::write(&pkg, t.replace("path = [\"~/.tool-bin\"]", "path = []")).unwrap();
    let (_, text) = run(&b, &["status", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["data"]["counts"]["packages"]["missing"], 1, "{text}");
}

/// Script packages that name the same `lock` never run at the same time; without the key they
/// do (the shared file is rewritten by overlapping runs).
#[test]
fn script_packages_with_the_same_lock_never_run_concurrently() {
    let mk = |lock: &str| {
        let pkgs: Vec<(String, String)> = (1..=3)
            .map(|i| {
                (
                    format!("p{i}"),
                    format!(
                        "[providers.script]\n{lock}check = \"test -f $HOME/p{i}.done\"\nversionless = true\ninstall = \"echo start{i} >> $HOME/trace; sleep 0.4; echo end{i} >> $HOME/trace; touch $HOME/p{i}.done\"\n"
                    ),
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = pkgs.iter().map(|(n, b)| (n.as_str(), b.as_str())).collect();
        let b = sandbox(&refs);
        let (code, text) = run(&b, &["apply", "--yes", "--packages-only"]);
        assert_eq!(code, 0, "{text}");
        std::fs::read_to_string(b.home.join("trace")).unwrap()
    };
    let serialized = mk("lock = \"pi\"\n");
    let lines: Vec<&str> = serialized.lines().collect();
    assert_eq!(lines.len(), 6, "{serialized}");
    for pair in lines.chunks(2) {
        assert_eq!(
            pair[0].trim_start_matches("start"),
            pair[1].trim_start_matches("end"),
            "each package finishes before the next starts: {serialized}"
        );
    }
    let free = mk("");
    let first_two: Vec<&str> = free.lines().take(2).collect();
    assert!(
        first_two.iter().all(|l| l.starts_with("start")),
        "without a lock they overlap: {free}"
    );
}
