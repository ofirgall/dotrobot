//! Performance regression: a plan over N brew packages with version constraints asks brew
//! about their latest versions in a couple of batched `brew info` calls, not one per package.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::path::Path;
use std::process::{Command, Stdio};

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Logs every invocation to `$BREW_LOG`; answers `info --json=v2 [--installed|--cask] names...`.
const FAKE_BREW: &str = r#"#!/bin/sh
echo "$*" >> "$BREW_LOG"
[ "$1" = info ] || exit 0
shift; shift
case "$1" in
  --installed) echo '{"formulae":[],"casks":[]}'; exit 0 ;;
  --cask) shift; kind=casks ;;
  *) kind=formulae ;;
esac
out=""
for n in "$@"; do
  [ -n "$out" ] && out="$out,"
  if [ $kind = casks ]; then
    out="$out{\"token\":\"$n\",\"full_token\":\"$n\",\"version\":\"9.0\"}"
  else
    out="$out{\"name\":\"$n\",\"full_name\":\"$n\",\"tap\":\"homebrew/core\",\"versions\":{\"stable\":\"9.0\"},\"installed\":[]}"
  fi
done
if [ $kind = casks ]; then echo "{\"formulae\":[],\"casks\":[$out]}"; else echo "{\"formulae\":[$out],\"casks\":[]}"; fi
"#;

#[test]
#[cfg(unix)]
fn plan_asks_brew_for_latest_versions_in_batches() {
    use std::os::unix::fs::PermissionsExt;
    const N: usize = 12;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    write(&root.join("dots.toml"), "schema = 1\nname = \"t\"\n");
    let mut names = Vec::new();
    for i in 0..N {
        let name = format!("pkg{i}");
        let cask = if i % 4 == 0 { "cask = true\n" } else { "" };
        write(
            &root.join(format!("packages/{name}/package.toml")),
            &format!("schema = 1\nname = \"{name}\"\nversion = \">=1\"\n[providers.brew]\n{cask}"),
        );
        names.push(format!("\"{name}\""));
    }
    write(
        &root.join("bundles/all.toml"),
        &format!("schema = 1\npackages = [{}]\n", names.join(", ")),
    );
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let prefix = tmp.path().join("brew");
    let brew = prefix.join("bin/brew");
    write(&brew, FAKE_BREW);
    std::fs::set_permissions(&brew, std::fs::Permissions::from_mode(0o755)).unwrap();
    let machine = tmp.path().join("machine.toml");
    write(
        &machine,
        &format!(
            "schema = 1\nroot = {:?}\nbundles = [\"all\"]\n",
            root.display().to_string()
        ),
    );
    let log = tmp.path().join("brew.log");
    let o = Command::new(env!("CARGO_BIN_EXE_dots"))
        .env_clear()
        .envs(dots_testkit::system_env())
        .env("PATH", dots_testkit::system_path())
        .env("SHELL", "/bin/zsh")
        .env("DOTS_PROVIDERS", "brew")
        .env("HOME", &home)
        .env("HOMEBREW_PREFIX", &prefix)
        .env("BREW_LOG", &log)
        .current_dir(tmp.path())
        .arg("--home")
        .arg(&home)
        .arg("--machine-config")
        .arg(&machine)
        .arg("plan")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let infos: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("info") && !l.contains("--installed"))
        .collect();
    assert!(
        !infos.is_empty(),
        "no latest query happened:\n{text}\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    // One call per kind (formulae, casks) for the whole plan.
    assert!(
        infos.len() <= 2,
        "{} info calls for {N} packages:\n{text}",
        infos.len()
    );
}
