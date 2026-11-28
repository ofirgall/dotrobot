//! Local (`path = ...`) sources for cargo and uv.
//!
//! A path source's version is `<crate version>.<commits touching the dir>+<short rev>`, so a
//! commit that changes the directory makes the package outdated (the engine compares the
//! numeric part; `+rev` is for people). What was installed is remembered in a small sidecar
//! file next to the backend's own state, because neither backend records the repo revision.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{Cmd, Ctx, Spec};

const GIT: &[&str] = &[
    "/usr/bin/git",
    "/opt/homebrew/bin/git",
    "/usr/local/bin/git",
];

/// The absolute source directory of a `path` option: relative paths are relative to `pkg_dir`
/// (the package directory), which the caller passes as the `pkg_dir` option.
pub(crate) fn source_dir(spec: &Spec) -> Option<PathBuf> {
    let p = Path::new(spec.get("path")?);
    Some(match spec.get("pkg_dir") {
        Some(base) if p.is_relative() => Path::new(base).join(p),
        _ => p.to_path_buf(),
    })
}

/// `version = "x"` under `[package]` (Cargo.toml) or `[project]` (pyproject.toml).
pub(crate) fn crate_version(dir: &Path) -> Option<String> {
    for (file, table) in [("Cargo.toml", "package"), ("pyproject.toml", "project")] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else {
            continue;
        };
        let mut inside = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                inside = t.trim_matches(['[', ']']).trim() == table;
            } else if inside
                && let Some(rest) = t.strip_prefix("version")
                && let Some(v) = rest.trim_start().strip_prefix('=')
            {
                let v = v.trim().trim_matches(['"', '\'']);
                if !v.is_empty() && !v.contains(['{', ' ']) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn git_bin() -> Option<PathBuf> {
    // The same lookup the git provider does (Windows locations included).
    crate::git::resolve_bin(Path::new(""))
        .or_else(|| GIT.iter().map(PathBuf::from).find(|p| p.is_file()))
}

/// The full version of the source in `dir` right now. Without git, just the crate version.
pub(crate) fn source_version(ctx: &Ctx, dir: &Path) -> Option<String> {
    let base = crate_version(dir)?;
    let Some(git) = git_bin() else {
        return Some(base);
    };
    let run = |args: &[&str]| -> Option<String> {
        let cmd = Cmd::new(&git)
            .args(["-C", &dir.to_string_lossy()])
            .args(args.iter().copied())
            .envs(&ctx.env);
        let out = ctx.runner.run(&cmd).ok().filter(|o| o.success())?;
        let s = out.stdout.trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    match (
        run(&["rev-list", "--count", "HEAD", "--", "."]),
        run(&["log", "-1", "--format=%h", "--", "."]),
    ) {
        (Some(n), Some(rev)) if n.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("{base}.{n}+{rev}"))
        }
        _ => Some(base),
    }
}

/// Installed versions of path sources, by package name.
pub(crate) fn read_revs(file: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub(crate) fn record_rev(file: &Path, name: &str, version: &str) {
    let mut m = read_revs(file);
    m.insert(name.to_string(), version.to_string());
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, serde_json::to_string_pretty(&m).unwrap_or_default());
}

pub(crate) fn forget_rev(file: &Path, name: &str) {
    let mut m = read_revs(file);
    if m.remove(name).is_some() {
        let _ = std::fs::write(file, serde_json::to_string_pretty(&m).unwrap_or_default());
    }
}
