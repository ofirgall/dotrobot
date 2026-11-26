use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use dots_core::paths::Dirs;
use dots_env::*;
use dots_model::EnvSpec;

fn spec(path: &[&str], vars: &[(&str, &str)], source: &[&str]) -> EnvSpec {
    EnvSpec {
        path: path.iter().map(|s| s.to_string()).collect(),
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        source: source.iter().map(|s| s.to_string()).collect(),
    }
}

fn ctx(home: &Path) -> EnvCtx {
    EnvCtx::new(home)
}

fn sample(home: &Path) -> EnvFiles {
    let c = vec![
        Contribution::new(
            "rust",
            spec(
                &["~/.cargo/bin"],
                &[("CARGO_HOME", "~/.cargo")],
                &["~/.cargo/env", "~/x.fish", "~/y.ps1"],
            ),
        ),
        Contribution::new(
            "npm",
            spec(
                &["~/.npm-packages/bin", "~/.cargo/bin"],
                &[
                    ("NPM_CONFIG_PREFIX", "~/.npm-packages"),
                    ("GREETING", "a \"b\" $c `d` \\e"),
                ],
                &[],
            ),
        ),
    ];
    generate(&c, &ctx(home)).unwrap()
}

fn golden(name: &str, actual: &str) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, actual).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        actual,
        "golden {name}"
    );
}

// Scenario: Cargo bin on PATH
#[test]
fn cargo_bin_on_path() {
    let home = Path::new("/home/u");
    let f = generate(
        &[Contribution::new("rust", spec(&["~/.cargo/bin"], &[], &[]))],
        &ctx(home),
    )
    .unwrap();
    assert!(f.sh.contains("__dots_path \"$HOME/.cargo/bin\""));
    assert!(f.fish.contains("set -gx PATH \"$HOME/.cargo/bin\" $PATH"));
    assert!(f.ps1.contains("$env:PATH = \"$HOME/.cargo/bin\""));
}

#[test]
fn golden_outputs() {
    let f = sample(Path::new("/home/u"));
    golden("env.sh", &f.sh);
    golden("env.fish", &f.fish);
    golden("env.ps1", &f.ps1);
    assert_eq!(f, sample(Path::new("/home/u")), "deterministic");
}

#[test]
fn later_package_wins_and_path_deduped() {
    let o = EnvOverlay::from_contributions(
        ctx(Path::new("/h")),
        &[
            Contribution::new("a", spec(&["/a", "/shared"], &[("V", "1")], &[])),
            Contribution::new("b", spec(&["/b", "/shared"], &[("V", "2")], &[])),
        ],
    );
    assert_eq!(o.path_entries(), ["/b", "/shared", "/a"]);
    assert_eq!(o.vars()["V"], "2");
}

#[test]
fn bad_var_name_rejected() {
    let r = generate(
        &[Contribution::new("a", spec(&[], &[("A B", "x")], &[]))],
        &ctx(Path::new("/h")),
    );
    assert!(matches!(r, Err(EnvError::BadVarName(_))));
}

// Scenario: Removed package
#[test]
fn removed_package_drops_out() {
    let t = tempfile::tempdir().unwrap();
    let dirs = Dirs::resolve(Some(t.path()), &dots_core::env::MapEnv::new());
    let c = ctx(&dirs.home);
    let with = vec![Contribution::new(
        "npm",
        spec(&[], &[("NPM_CONFIG_PREFIX", "~/.npm-packages")], &[]),
    )];
    let rc = t.path().join(".zshenv");
    sync(&dirs, &c, &with, Shell::Zsh, Some(&rc)).unwrap();
    for ext in ["sh", "fish", "ps1"] {
        assert!(
            std::fs::read_to_string(dirs.env_file(ext))
                .unwrap()
                .contains("NPM_CONFIG_PREFIX")
        );
    }
    sync(&dirs, &c, &[], Shell::Zsh, Some(&rc)).unwrap();
    for ext in ["sh", "fish", "ps1"] {
        assert!(
            !std::fs::read_to_string(dirs.env_file(ext))
                .unwrap()
                .contains("NPM_CONFIG_PREFIX")
        );
    }
}

// Scenario: Idempotent inject (+ text outside markers untouched)
#[test]
fn inject_idempotent_and_preserves_text() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let rc = home.join(".zshenv");
    let before = "export A=1\n# my comment\nalias x=y";
    std::fs::write(&rc, before).unwrap();
    let envf = home.join(".local/state/dots/env.sh");
    assert_eq!(
        inject_rc(&rc, Shell::Zsh, &envf, home).unwrap(),
        InjectOutcome::Updated
    );
    let once = std::fs::read_to_string(&rc).unwrap();
    assert_eq!(
        inject_rc(&rc, Shell::Zsh, &envf, home).unwrap(),
        InjectOutcome::Unchanged
    );
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), once);
    assert_eq!(once.matches(MARKER_OPEN).count(), 1);
    assert_eq!(once.matches(MARKER_CLOSE).count(), 1);
    assert!(once.starts_with(&format!("{before}\n")));
    assert!(once.contains(
        "[ -f \"$HOME/.local/state/dots/env.sh\" ] && . \"$HOME/.local/state/dots/env.sh\""
    ));
    // text after the block and duplicate blocks
    let dup = format!("top\n{once}middle\n{once}bottom\n");
    std::fs::write(&rc, &dup).unwrap();
    inject_rc(&rc, Shell::Zsh, &envf, home).unwrap();
    let fixed = std::fs::read_to_string(&rc).unwrap();
    assert_eq!(fixed.matches(MARKER_OPEN).count(), 1);
    assert!(
        fixed.starts_with("top\n") && fixed.contains("middle\n") && fixed.ends_with("bottom\n")
    );
}

#[test]
fn inject_creates_and_refuses_unterminated() {
    let t = tempfile::tempdir().unwrap();
    let rc = t.path().join("sub/.bashrc");
    std::fs::create_dir_all(rc.parent().unwrap()).unwrap();
    let ef = t.path().join("env.sh");
    assert_eq!(
        inject_rc(&rc, Shell::Bash, &ef, Path::new("/nope")).unwrap(),
        InjectOutcome::Created
    );
    std::fs::write(&rc, format!("keep\n{MARKER_OPEN}\nhalf\n")).unwrap();
    assert!(matches!(
        inject_rc(&rc, Shell::Bash, &ef, t.path()),
        Err(EnvError::UnterminatedBlock(_))
    ));
    assert_eq!(
        std::fs::read_to_string(&rc).unwrap(),
        format!("keep\n{MARKER_OPEN}\nhalf\n")
    );
}

// Scenario: Fresh tool on PATH
#[test]
fn overlay_exposes_fresh_tool_to_later_actions() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join(".cargo/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\necho fresh-cargo\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut o = EnvOverlay::new(ctx(t.path()));
    let base = std::ffi::OsString::from("/usr/bin:/bin");
    assert!(!o.runner_env(Some(&base)).contains_key("PATH"));
    o.add_package(&spec(&["~/.cargo/bin"], &[("CARGO_HOME", "~/.cargo")], &[]));
    let env = o.runner_env(Some(&base));
    assert!(env["PATH"].starts_with(&bin.to_string_lossy().into_owned()));
    assert!(env["PATH"].ends_with("/usr/bin:/bin"));
    assert_eq!(env["CARGO_HOME"], t.path().join(".cargo").to_string_lossy());
    #[cfg(unix)]
    {
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg("cargo")
            .env("PATH", &env["PATH"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "fresh-cargo");
    }
}

#[test]
fn shell_smoke() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    let f = sample(home);
    std::fs::write(home.join("env.sh"), &f.sh).unwrap();
    std::fs::write(home.join("env.fish"), &f.fish).unwrap();
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(". \"$0\"; . \"$0\"; echo \"$PATH\"; printf %s \"$GREETING\"")
        .arg(home.join("env.sh"))
        .env("HOME", "/home/u")
        .env("PATH", "/usr/bin")
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    let (path, greeting) = s.split_once('\n').unwrap();
    assert_eq!(
        path,
        "/home/u/.npm-packages/bin:/home/u/.cargo/bin:/usr/bin"
    );
    assert_eq!(greeting, "a \"b\" $c `d` \\e");
    if let Ok(o) = Command::new("fish").arg("--version").output()
        && o.status.success()
    {
        let out = Command::new("fish")
            .arg("-c")
            .arg(format!(
                "source {}; echo $PATH; echo $GREETING",
                home.join("env.fish").display()
            ))
            .env("HOME", "/home/u")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout);
        assert!(s.contains("/home/u/.cargo/bin"));
        assert!(s.contains("a \"b\" $c `d` \\e"));
    }
}
