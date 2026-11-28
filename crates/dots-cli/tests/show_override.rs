//! Task 5.7: `show --effective`, `override list|edit|diff`, the status customization marker
//! and the stale/redundant override lint, in a `--home` sandbox.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))] // helpers of unix-only tests

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

impl Out {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| panic!("{e}: {}", self.stdout))
    }
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    tmp: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

const GHOSTTY: &str = r#"schema = 1
name = "ghostty"
version = "1.1"

[[files]]
dst = "~/.config/ghostty/config"
src = "config"

[settings.font_size]
type = "int"
default = 13
"#;

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let base = dots_testkit::canonical(tmp.path());
        let repo = base.join("repo");
        let home = base.join("home");
        fs::create_dir_all(&home).unwrap();
        write(
            &repo,
            "dots.toml",
            "schema = 1\n[priority]\nany = [\"cargo\"]\n",
        );
        write(
            &repo,
            "bundles/cli.toml",
            "schema = 1\npackages = [\"ghostty\", \"delta\"]\n",
        );
        write(&repo, "packages/ghostty/package.toml", GHOSTTY);
        write(&repo, "packages/ghostty/config", "x\n");
        write(
            &repo,
            "packages/delta/package.toml",
            "schema = 1\nname = \"delta\"\n",
        );
        write(
            &base,
            "machine.toml",
            &format!(
                "schema = 1\nroot = {:?}\nbundles = [\"cli\"]\n",
                repo.display().to_string()
            ),
        );
        Sandbox {
            _tmp: tmp,
            tmp: base,
            repo,
            home,
        }
    }

    fn machine_overrides(&self) -> PathBuf {
        self.home.join(".config/dots/overrides")
    }

    fn run_with(&self, editor: Option<&Path>, args: &[&str]) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dots"));
        cmd.env_clear()
            .envs(dots_testkit::system_env())
            .env("PATH", dots_testkit::system_path())
            .env("DOTS_PROVIDERS", "none")
            .env("HOME", self.tmp.join("sentinel"))
            .current_dir(&self.tmp)
            .arg("--home")
            .arg(&self.home)
            .arg("--machine-config")
            .arg(self.tmp.join("machine.toml"))
            .args(args);
        if let Some(e) = editor {
            cmd.env("EDITOR", e);
        }
        let out = cmd.output().unwrap();
        Out {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code().unwrap(),
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        self.run_with(None, args)
    }

    /// An `$EDITOR` that appends `text` to the file it is given.
    fn editor(&self, text: &str) -> PathBuf {
        let p = self.tmp.join(format!("editor-{}.sh", text.len()));
        fs::write(
            &p,
            format!("#!/bin/sh\nprintf '%s\\n' '{text}' >> \"$1\"\n"),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

#[test]
fn annotated_output_names_the_layer_of_every_key() {
    let s = Sandbox::new();
    write(
        &s.repo,
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.2\"\n",
    );
    write(
        &s.machine_overrides(),
        "packages/ghostty/override.toml",
        "schema = 1\n[settings.font_size]\ndefault = 15\n",
    );
    let o = s.run(&["show", "pkg:ghostty", "--effective", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stdout.contains("version = \"1.2\"")
            && o.stdout
                .contains("repo override (overrides/packages/ghostty/override.toml)"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("machine override (~/.config/dots/overrides/packages/ghostty/override.toml)"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("definition (packages/ghostty/package.toml)"),
        "{}",
        o.stdout
    );

    let j = s
        .run(&["show", "pkg:ghostty", "--effective", "--json"])
        .json();
    assert_eq!(j["data"]["effective"], true);
    assert_eq!(j["data"]["value"]["version"], "1.2");
    let prov = j["data"]["provenance"].as_array().unwrap();
    let version = prov.iter().find(|p| p["path"] == "version").unwrap();
    assert_eq!(version["scope"], "repo");
    assert_eq!(version["line"], 2);
    let fs = prov
        .iter()
        .find(|p| p["path"] == "settings.font_size.default")
        .unwrap();
    assert_eq!(fs["scope"], "machine");
}

#[test]
fn show_by_address_prints_one_setting_and_plain_show_is_the_definition() {
    let s = Sandbox::new();
    write(
        &s.repo,
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.2\"\n[settings.font_size]\ndefault = 15\n",
    );
    let o = s.run(&["show", "pkg:ghostty/setting:font_size", "--color", "never"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(
        o.stdout.contains("default = 13") && !o.stdout.contains("repo override"),
        "{}",
        o.stdout
    );
    let e = s.run(&[
        "show",
        "pkg:ghostty/setting:font_size",
        "--effective",
        "--color",
        "never",
    ]);
    assert!(
        e.stdout.contains("default = 15") && e.stdout.contains("repo override"),
        "{}",
        e.stdout
    );
    let plain = s.run(&["show", "pkg:ghostty", "--color", "never"]);
    assert!(
        plain.stdout.contains("version = \"1.1\""),
        "{}",
        plain.stdout
    );

    let bad = s.run(&["show", "pkg:ghosty", "--json"]);
    assert_eq!(bad.code, 1);
    let j = bad.json();
    assert_eq!(j["errors"][0]["code"], "address-not-found");
    assert!(
        j["errors"][0]["hint"]
            .as_str()
            .unwrap()
            .contains("pkg:ghostty")
    );
    let nonsense = s.run(&["show", "nonsense", "--json"]);
    assert_eq!(nonsense.json()["errors"][0]["code"], "address-invalid");
}

#[test]
#[cfg(unix)]
fn scaffold_creates_the_machine_override_and_opens_the_editor() {
    let s = Sandbox::new();
    let ed = s.editor("# edited");
    let o = s.run_with(
        Some(&ed),
        &["override", "edit", "pkg:delta", "--machine", "--json"],
    );
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    let path = s.machine_overrides().join("packages/delta/override.toml");
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("schema = 1\n") && text.contains("# edited"),
        "{text}"
    );
    let j = o.json();
    assert_eq!(j["data"]["created"], true);
    assert_eq!(j["data"]["scope"], "machine");
    // A second run reuses the file.
    let again = s.run_with(
        Some(&ed),
        &["override", "edit", "pkg:delta", "--machine", "--json"],
    );
    assert_eq!(again.json()["data"]["created"], false);
    assert_eq!(
        fs::read_to_string(&path)
            .unwrap()
            .matches("# edited")
            .count(),
        2
    );
    // Without --machine the file goes into the repo.
    let r = s.run_with(
        Some(&ed),
        &[
            "override",
            "edit",
            "pkg:ghostty/file:~/.config/ghostty/config",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let repo_file =
        fs::read_to_string(s.repo.join("overrides/packages/ghostty/override.toml")).unwrap();
    assert!(
        repo_file.contains("[files.\"~/.config/ghostty/config\"]"),
        "{repo_file}"
    );
    // Unknown objects and a failing editor are errors.
    let missing = s.run_with(Some(&ed), &["override", "edit", "pkg:nope"]);
    assert_eq!(missing.code, 1);
    let fail = s.tmp.join("fail.sh");
    fs::write(&fail, "#!/bin/sh\nexit 3\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&fail, fs::Permissions::from_mode(0o755)).unwrap();
    let f = s.run_with(Some(&fail), &["override", "edit", "pkg:delta", "--json"]);
    assert_eq!(f.json()["errors"][0]["code"], "editor-failed");
}

#[test]
fn override_list_groups_by_scope_and_diff_is_structural() {
    let s = Sandbox::new();
    write(
        &s.repo,
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.2\"\n[settings.font_size]\ndefault = 15\n",
    );
    write(
        &s.repo,
        "overrides/bundles/cli.toml",
        "schema = 1\npackages = { remove = [\"delta\"] }\n",
    );
    write(
        &s.machine_overrides(),
        "packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.3\"\n",
    );
    let l = s.run(&["override", "list", "--json"]).json();
    let scopes = l["data"]["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 2);
    assert_eq!(scopes[0]["scope"], "repo");
    assert_eq!(scopes[0]["overrides"].as_array().unwrap().len(), 2);
    assert_eq!(scopes[1]["scope"], "machine");
    assert_eq!(scopes[1]["overrides"][0]["target"], "pkg:ghostty");
    let human = s.run(&["override", "list", "--color", "never"]);
    assert!(
        human.stdout.contains("repo (overrides)")
            && human.stdout.contains("machine (~/.config/dots/overrides)"),
        "{}",
        human.stdout
    );

    let d = s.run(&["override", "diff", "pkg:ghostty", "--json"]).json();
    let changes = d["data"]["changes"].as_array().unwrap();
    let version = changes.iter().find(|c| c["path"] == "version").unwrap();
    assert_eq!(
        (version["before"].clone(), version["after"].clone()),
        ("1.1".into(), "1.3".into())
    );
    assert_eq!(version["change"], "changed");
    assert!(
        version["source"]
            .as_str()
            .unwrap()
            .starts_with("machine override")
    );
    assert!(
        changes
            .iter()
            .any(|c| c["path"] == "settings.font_size.default" && c["after"] == 15)
    );
    let b = s.run(&["override", "diff", "bundle:cli", "--json"]).json();
    let removed = b["data"]["changes"].as_array().unwrap();
    assert!(
        removed
            .iter()
            .any(|c| c["change"] == "removed" && c["before"] == "delta"),
        "{b}"
    );
    let none = s.run(&["override", "diff", "pkg:delta", "--color", "never"]);
    assert!(
        none.stdout.contains("no overrides change it"),
        "{}",
        none.stdout
    );
}

#[test]
fn overridden_package_is_marked_in_status() {
    let s = Sandbox::new();
    write(
        &s.repo,
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.2\"\n",
    );
    write(
        &s.machine_overrides(),
        "packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.3\"\n",
    );
    let o = s.run(&["status", "--all", "--color", "never"]);
    assert!(
        o.stdout.contains("✎ overridden (repo, machine)"),
        "{}\n{}",
        o.stdout,
        o.stderr
    );
    let j = s.run(&["status", "--all", "--json"]).json();
    let pkgs = j["data"]["items"].as_array().cloned().unwrap_or_default();
    let g = pkgs
        .iter()
        .find(|i| i["name"] == "ghostty" && i["area"] == "packages");
    assert_eq!(
        g.map(|g| g["overridden"].clone()),
        Some(serde_json::json!(["repo", "machine"])),
        "{j}"
    );
}

#[test]
fn renamed_target_is_a_stale_override_with_the_new_address() {
    let s = Sandbox::new();
    // The override targets a file entry that upstream has since renamed.
    write(
        &s.repo,
        "overrides/packages/ghostty/override.toml",
        "schema = 1\n[files.\"~/.config/ghostty/old\"]\nmode = \"copy\"\n",
    );
    let o = s.run(&["lint", "--json"]);
    assert_eq!(o.code, 1, "{}", o.stdout);
    let e = &o.json()["errors"][0];
    assert_eq!(e["code"], "stale-override");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("pkg:ghostty/file:~/.config/ghostty/old"),
        "{e}"
    );
    assert!(
        e["hint"]
            .as_str()
            .unwrap()
            .contains("pkg:ghostty/file:~/.config/ghostty/config"),
        "{e}"
    );
    assert_eq!(
        e["provenance"][0]["file"],
        "overrides/packages/ghostty/override.toml"
    );
}

#[test]
fn redundant_override_is_a_warning() {
    let s = Sandbox::new();
    write(
        &s.repo,
        "overrides/packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.1\"\n[settings.font_size]\ndefault = 13\n",
    );
    write(
        &s.machine_overrides(),
        "packages/ghostty/override.toml",
        "schema = 1\nversion = \"1.2\"\n[settings.font_size]\ndefault = 13\n",
    );
    let o = s.run(&["lint", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stdout);
    let w = o.json()["warnings"].clone();
    let msgs: Vec<String> = w
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "redundant-override")
        .map(|d| d["message"].as_str().unwrap().to_string())
        .collect();
    // The repo version and font size repeat the definition; the machine font size repeats
    // the repo layer; the machine version is a real change.
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("version")));
    assert_eq!(msgs.iter().filter(|m| m.contains("font_size")).count(), 2);
}

fn redundant_count(o: &Out) -> usize {
    o.json()["warnings"].as_array().map_or(0, |w| {
        w.iter()
            .filter(|d| d["code"] == "redundant-override")
            .count()
    })
}

fn plat() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

#[test]
fn redundant_override_lint_compares_with_the_platform_table_beneath() {
    // Definition says 1.1, this platform's table says 2.0.
    let with = |ov: &str| {
        let s = Sandbox::new();
        write(
            &s.repo,
            "packages/ghostty/package.toml",
            &format!("{GHOSTTY}\n[platform.{}]\nversion = \"2.0\"\n", plat()),
        );
        write(
            &s.repo,
            "overrides/packages/ghostty/override.toml",
            &format!("schema = 1\nversion = \"{ov}\"\n"),
        );
        redundant_count(&s.run(&["lint", "--json"]))
    };
    // 1.1 only equals the definition: on this machine it changes 2.0, so it is not redundant.
    assert_eq!(with("1.1"), 0);
    // 2.0 is what the platform table already gives.
    assert_eq!(with("2.0"), 1);
}

#[test]
#[cfg(unix)]
fn override_edit_removes_a_new_scaffold_when_nothing_was_changed_or_the_editor_fails() {
    let s = Sandbox::new();
    let file = s.repo.join("overrides/packages/ghostty/override.toml");
    let noop = s.tmp.join("noop.sh");
    fs::write(&noop, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&noop, fs::Permissions::from_mode(0o755)).unwrap();
    let o = s.run_with(
        Some(&noop),
        &["override", "edit", "pkg:ghostty/provider:brew"],
    );
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert!(!file.exists(), "unchanged scaffold was left behind");

    let bad = s.tmp.join("bad.sh");
    fs::write(&bad, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o755)).unwrap();
    let o = s.run_with(Some(&bad), &["override", "edit", "pkg:ghostty"]);
    assert_ne!(o.code, 0);
    assert!(!file.exists(), "scaffold left behind after a failed editor");

    // An edit that adds something keeps the file.
    let ed = s.editor("version = \"9\"");
    let o = s.run_with(Some(&ed), &["override", "edit", "pkg:ghostty"]);
    assert_eq!(o.code, 0, "{} {}", o.stdout, o.stderr);
    assert!(file.exists());
}
