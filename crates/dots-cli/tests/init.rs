//! Task 4.11: `dots init` end to end in a `--home` sandbox (no writes outside it, no network).

use std::path::{Path, PathBuf};
use std::process::Command;

use dots_model::{MachineConfig, Repo};
use serde_json::Value;

struct Sandbox {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    machine: PathBuf,
}

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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

impl Sandbox {
    fn new(hostname: &str) -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // A fake `hostname` first on PATH pins the detected host.
        let bin = base.join("bin");
        write(
            &bin.join("hostname"),
            &format!("#!/bin/sh\necho {hostname}\n"),
        );
        Command::new("chmod")
            .arg("+x")
            .arg(bin.join("hostname"))
            .status()
            .unwrap();
        Sandbox {
            machine: base.join("config/machine.toml"),
            _tmp: tmp,
            base,
            home,
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_dots"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.base.join("bin").display()),
            )
            .env("DOTS_PROVIDERS", "none")
            .env("HOME", self.base.join("sentinel"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .current_dir(&self.base)
            .arg("--home")
            .arg(&self.home)
            .arg("--machine-config")
            .arg(&self.machine)
            .args(args)
            .output()
            .unwrap();
        Out {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.home.join("dotfiles")
    }

    /// Nothing may be written outside the sandbox base (the sentinel HOME stays absent).
    fn assert_contained(&self) {
        assert!(!self.base.join("sentinel").exists());
    }
}

/// A repo with a recommended bundle, a platform-only bundle, tags and a profile.
fn seed_repo(dir: &Path) {
    write(
        &dir.join("dots.toml"),
        "schema = 1\nname = \"seed\"\n[tags]\nwork = { description = \"work\", recommended = { macos = true } }\nremote = \"headless\"\n",
    );
    write(
        &dir.join("bundles/base.toml"),
        "schema = 1\nrecommended = true\npackages = [\"git\"]\n",
    );
    write(
        &dir.join("bundles/mac-desktop.toml"),
        "schema = 1\npackages = []\n[platform.macos]\nrecommended = true\n",
    );
    write(
        &dir.join("bundles/extra.toml"),
        "schema = 1\npackages = []\n",
    );
    write(
        &dir.join("packages/git/package.toml"),
        "schema = 1\nname = \"git\"\n[settings.name]\ntype = \"string\"\ndefault = \"me\"\nask = true\n",
    );
    write(
        &dir.join("machines/work-mbp.toml"),
        "schema = 1\nbundles = [\"base\", \"extra\"]\ntags = [\"work\"]\n",
    );
}

fn bare_remote(sb: &Sandbox) -> PathBuf {
    let work = sb.base.join("seed");
    seed_repo(&work);
    let git = |dir: &Path, args: &[&str]| {
        let s = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(s.success(), "git {args:?}");
    };
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "seed"]);
    let bare = sb.base.join("remote.git");
    git(
        &sb.base,
        &[
            "clone",
            "-q",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    bare
}

#[test]
fn new_repo_is_valid_and_lint_is_clean() {
    let sb = Sandbox::new("plain-host");
    let out = sb.run(&["init", "--defaults", "--no-input", "--json"]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let doc = out.json();
    assert_eq!(doc["data"]["created"], true);
    let root = sb.root();
    assert!(root.join("dots.toml").is_file());
    assert!(root.join("bundles").is_dir() && root.join("packages").is_dir());
    // Strict parsers accept what was generated.
    let repo = Repo::load(&root).unwrap().value;
    assert_eq!(repo.manifest.schema, 1);
    let m = MachineConfig::load(&root, &sb.machine, "plain-host")
        .unwrap()
        .value;
    assert!(m.bundles.is_empty() && m.profile.is_none());
    let lint = sb.run(&["lint"]);
    assert_eq!(lint.code, 0, "{}{}", lint.stdout, lint.stderr);
    assert!(lint.stdout.contains("lint: ok"), "{}", lint.stdout);
    sb.assert_contained();
}

#[test]
fn non_interactive_init_records_recommended_bundles_and_tags() {
    let sb = Sandbox::new("plain-host");
    seed_repo(&sb.root());
    let out = sb.run(&["init", "--defaults", "--no-input"]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(
        !sb.root().join("bundles/.gitkeep").exists(),
        "existing repo untouched"
    );
    let m = MachineConfig::load(&sb.root(), &sb.machine, "plain-host")
        .unwrap()
        .value;
    // `base` is recommended everywhere, `mac-desktop` only on macos.
    let mac = cfg!(target_os = "macos");
    let mut want = vec!["base".to_string()];
    if mac {
        want.push("mac-desktop".into());
    }
    assert_eq!(m.bundles, want);
    assert_eq!(
        m.tags,
        if mac {
            vec!["work".to_string()]
        } else {
            vec![]
        }
    );
    assert!(m.settings.is_empty(), "ask settings keep their default");
    sb.assert_contained();
}

#[test]
fn required_setting_without_answer_fails_with_hint_and_writes_nothing() {
    let sb = Sandbox::new("plain-host");
    seed_repo(&sb.root());
    write(
        &sb.root().join("packages/git/package.toml"),
        "schema = 1\nname = \"git\"\n[settings.email]\ntype = \"string\"\nrequired = true\n",
    );
    let out = sb.run(&["init", "--defaults", "--json"]);
    assert_eq!(out.code, 1);
    let doc = out.json();
    assert_eq!(doc["errors"][0]["code"], "init-answers-needed");
    assert!(
        doc["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("git.email")
    );
    assert!(!doc["errors"][0]["hint"].as_str().unwrap().is_empty());
    assert!(!sb.machine.exists());
}

#[test]
fn refuses_to_overwrite_machine_toml() {
    let sb = Sandbox::new("plain-host");
    write(&sb.machine, "schema = 1\n");
    let out = sb.run(&["init", "--defaults", "--json"]);
    assert_eq!(out.code, 1);
    assert_eq!(out.json()["errors"][0]["code"], "init-machine-exists");
    assert_eq!(
        std::fs::read_to_string(&sb.machine).unwrap(),
        "schema = 1\n"
    );
}

#[test]
fn from_clones_and_picks_the_matching_profile() {
    let sb = Sandbox::new("work-mbp");
    let remote = bare_remote(&sb);
    let out = sb.run(&[
        "init",
        "--from",
        remote.to_str().unwrap(),
        "--defaults",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    let doc = out.json();
    assert_eq!(doc["data"]["profile"], "work-mbp");
    assert_eq!(doc["data"]["cloned_from"], remote.to_str().unwrap());
    assert!(sb.root().join(".git").exists());
    let text = std::fs::read_to_string(&sb.machine).unwrap();
    assert!(text.contains("profile = \"work-mbp\""), "{text}");
    // The profile already answers bundles and tags: they are not repeated.
    assert!(
        !text.contains("bundles") && !text.contains("tags"),
        "{text}"
    );
    let m = MachineConfig::load(&sb.root(), &sb.machine, "other")
        .unwrap()
        .value;
    assert_eq!(m.profile.as_deref(), Some("work-mbp"));
    assert_eq!(m.bundles, ["base", "extra"]);
    sb.assert_contained();
}

#[test]
fn from_without_a_matching_profile_uses_recommendations() {
    let sb = Sandbox::new("other-host");
    let remote = bare_remote(&sb);
    let out = sb.run(&[
        "init",
        "--from",
        remote.to_str().unwrap(),
        "--root",
        sb.base.join("repo").to_str().unwrap(),
        "--defaults",
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(out.json()["data"]["profile"], Value::Null);
    let text = std::fs::read_to_string(&sb.machine).unwrap();
    assert!(text.contains("root = "), "{text}");
    assert!(text.contains("bundles = [\"base\""), "{text}");
}

#[test]
fn from_failures_and_existing_root() {
    let sb = Sandbox::new("h");
    let out = sb.run(&[
        "init",
        "--from",
        sb.base.join("nope.git").to_str().unwrap(),
        "--defaults",
        "--json",
    ]);
    assert_eq!(out.code, 1);
    assert_eq!(out.json()["errors"][0]["code"], "clone-failed");
    write(&sb.root().join("x"), "");
    let remote = bare_remote(&sb);
    let out = sb.run(&[
        "init",
        "--from",
        remote.to_str().unwrap(),
        "--defaults",
        "--json",
    ]);
    assert_eq!(out.json()["errors"][0]["code"], "init-root-exists");
    assert!(!sb.machine.exists());
}

#[test]
fn json_output_validates_against_the_schema() {
    let sb = Sandbox::new("work-mbp");
    let remote = bare_remote(&sb);
    let ok = sb.run(&[
        "init",
        "--from",
        remote.to_str().unwrap(),
        "--defaults",
        "--json",
    ]);
    let bad = sb.run(&["init", "--defaults", "--json"]); // machine.toml now exists
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/dots-init.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (out, ok_flag) in [(&ok, true), (&bad, false)] {
        assert!(out.stderr.is_empty(), "{}", out.stderr);
        let doc = out.json();
        if let Err(e) = validator.validate(&doc) {
            panic!("{e}: {doc}");
        }
        assert_eq!(doc["schema"], "dots/init@1");
        assert_eq!(doc["ok"], ok_flag);
    }
}
