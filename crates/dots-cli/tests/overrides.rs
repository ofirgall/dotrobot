//! Tasks 5.1 and 5.6 end to end: platform tables and the three override locations, in a
//! `--home` sandbox.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Out {
    stdout: String,
    stderr: String,
    code: i32,
}

/// A sandbox: `home/` (config dir `home/.config/dots`), a repo, and a machine.toml.
struct Box {
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

impl Box {
    fn new() -> Box {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
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
            "schema = 1\npackages = [\"alpha\", \"beta\"]\n",
        );
        for p in ["alpha", "beta", "gamma"] {
            write(
                &repo,
                &format!("packages/{p}/package.toml"),
                &format!("schema = 1\nname = \"{p}\"\n"),
            );
        }
        write(
            &base,
            "machine.toml",
            &format!(
                "schema = 1\nroot = {:?}\nbundles = [\"cli\"]\n",
                repo.display().to_string()
            ),
        );
        Box {
            _tmp: tmp,
            tmp: base,
            repo,
            home,
        }
    }

    fn machine_overrides(&self) -> PathBuf {
        self.home.join(".config/dots/overrides")
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_dots"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("DOTS_PROVIDERS", "none")
            .env("HOME", self.tmp.join("sentinel"))
            .current_dir(&self.tmp)
            .arg("--home")
            .arg(&self.home)
            .arg("--machine-config")
            .arg(self.tmp.join("machine.toml"))
            .args(args)
            .output()
            .unwrap();
        Out {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            code: out.status.code().unwrap(),
        }
    }

    fn why(&self, pkg: &str) -> Out {
        self.run(&["why", pkg, "--color", "never"])
    }
}

#[test]
fn edit_a_bundles_list_from_a_repo_override() {
    let b = Box::new();
    write(
        &b.repo,
        "overrides/bundles/cli.toml",
        "schema = 1\npackages = { add = [\"gamma\"], remove = [\"beta\"] }\n",
    );
    let g = b.why("gamma");
    assert_eq!(g.code, 0, "{}", g.stderr);
    assert!(
        g.stdout.contains("gamma: active") && g.stdout.contains("cli"),
        "{}",
        g.stdout
    );
    let beta = b.why("beta");
    assert!(!beta.stdout.contains("beta: active"), "{}", beta.stdout);
    // The committed bundle is untouched.
    assert!(
        fs::read_to_string(b.repo.join("bundles/cli.toml"))
            .unwrap()
            .contains("beta")
    );
}

#[test]
fn identity_field_in_an_override_fails_loading() {
    let b = Box::new();
    write(
        &b.repo,
        "overrides/packages/alpha/override.toml",
        "schema = 1\nname = \"x\"\n",
    );
    for args in [&["why", "alpha"][..], &["lint"][..]] {
        let o = b.run(args);
        assert_ne!(o.code, 0, "{args:?}");
        assert!(o.stderr.contains("override-identity-field"), "{}", o.stderr);
        assert!(
            o.stderr.contains("overrides/packages/alpha/override.toml"),
            "{}",
            o.stderr
        );
    }
}

#[test]
fn machine_only_tweak_beats_the_repo_and_stays_out_of_it() {
    let b = Box::new();
    write(
        &b.repo,
        "overrides/packages/alpha/override.toml",
        "schema = 1\nwhen = \"false\"\n",
    );
    let o = b.why("alpha");
    assert!(o.stdout.contains("alpha: inert"), "{}", o.stdout);

    write(
        &b.machine_overrides(),
        "packages/alpha/override.toml",
        "schema = 1\nwhen = \"true\"\n",
    );
    let o = b.why("alpha");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("alpha: active"), "{}", o.stdout);
    assert_eq!(
        fs::read_to_string(b.repo.join("overrides/packages/alpha/override.toml")).unwrap(),
        "schema = 1\nwhen = \"false\"\n"
    );
    assert!(!b.repo.join(".config").exists());
}

#[test]
fn machine_profile_overrides_sit_between_repo_and_machine() {
    let b = Box::new();
    write(
        &b.tmp,
        "machine.toml",
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"cli\"]\nprofile = \"laptop\"\n",
            b.repo.display().to_string()
        ),
    );
    write(&b.repo, "machines/laptop.toml", "schema = 1\n");
    write(
        &b.repo,
        "overrides/packages/alpha/override.toml",
        "schema = 1\nwhen = \"false\"\n",
    );
    write(
        &b.repo,
        "machines/laptop/overrides/packages/alpha/override.toml",
        "schema = 1\nwhen = \"true\"\n",
    );
    assert!(b.why("alpha").stdout.contains("alpha: active"));
    write(
        &b.machine_overrides(),
        "packages/alpha/override.toml",
        "schema = 1\nwhen = \"false\"\n",
    );
    assert!(b.why("alpha").stdout.contains("alpha: inert"));
}

#[test]
fn platform_table_can_change_whether_a_package_exists() {
    let b = Box::new();
    write(
        &b.repo,
        "packages/gamma/package.toml",
        "schema = 1\nname = \"gamma\"\nwhen = 'os = \"no-such-os\"'\n",
    );
    write(
        &b.repo,
        "bundles/cli.toml",
        "schema = 1\npackages = [\"alpha\", \"gamma\"]\n",
    );
    assert!(b.why("gamma").stdout.contains("gamma: inert"));
    // `any` is in every chain, so this table applies on every machine.
    write(
        &b.repo,
        "packages/gamma/package.toml",
        "schema = 1\nname = \"gamma\"\nwhen = 'os = \"no-such-os\"'\n[platform.any]\nwhen = \"true\"\n",
    );
    let o = b.why("gamma");
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("gamma: active"), "{}", o.stdout);
}

#[test]
fn unknown_platform_key_fails_lint_with_a_suggestion() {
    let b = Box::new();
    write(
        &b.repo,
        "packages/alpha/package.toml",
        "schema = 1\nname = \"alpha\"\n[platform.\"ubunut-22.04\"]\nprefer = [\"cargo\"]\n",
    );
    let o = b.run(&["lint"]);
    assert_ne!(o.code, 0);
    assert!(o.stderr.contains("unknown-platform"), "{}", o.stderr);
    assert!(o.stderr.contains("ubuntu-22.04"), "{}", o.stderr);
    assert!(
        o.stderr.contains("packages/alpha/package.toml:3"),
        "{}",
        o.stderr
    );
    // A key in an override file is checked the same way.
    write(
        &b.repo,
        "packages/alpha/package.toml",
        "schema = 1\nname = \"alpha\"\n",
    );
    write(
        &b.machine_overrides(),
        "packages/alpha/override.toml",
        "schema = 1\n[platform.\"ubunut\"]\nprefer = [\"cargo\"]\n",
    );
    let o = b.run(&["lint"]);
    assert_ne!(o.code, 0);
    assert!(o.stderr.contains("unknown-platform"), "{}", o.stderr);
}

#[test]
fn stale_override_target_is_an_error_naming_the_closest_address() {
    let b = Box::new();
    write(
        &b.machine_overrides(),
        "packages/alhpa/override.toml",
        "schema = 1\n",
    );
    let o = b.run(&["lint"]);
    assert_ne!(o.code, 0);
    assert!(
        o.stderr.contains("stale-override") && o.stderr.contains("pkg:alpha"),
        "{}",
        o.stderr
    );
}
