//! Provider shims: `[providers.apt] bin = "fdfind"  shim = "fd"` (ARCHITECTURE section 5.1
//! "Package file": "dots creates ~/.local/bin/fd -> fdfind and owns it"; 7.4 `dots bins` lists
//! it as `shim -> /usr/bin/fdfind`; package-resolution registers the shim name as a claimed
//! binary).
//!
//! The rule, where the specs are silent:
//!
//! - A shim is the symlink `~/.local/bin/<shim>` (`DOTS_BIN`) to the absolute path of the
//!   provider's `bin` (else the package's `bin`) found on `PATH` (the run's PATH, which
//!   includes what this run installed) or in the usual system bin dirs. A package that
//!   declares `shim` without any `bin` has nothing to point at and gets no shim.
//! - It is planned for an active package whose provider is chosen and which is installed (or
//!   about to be) when the link is missing or dangling. A link that already resolves is left
//!   alone. A real file or directory at the path is never replaced.
//! - It is created after the package installs and before files, recorded in `state.json`
//!   (`shims`, keyed by path) when dots made it, and removed (only while it is still a
//!   symlink) when its package is dropped or stops declaring it (full-scope runs, like
//!   orphaned files).
//! - `dots status` reports a missing or dangling shim as drift on its package.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use dots_model::Repo;
use dots_resolve::Resolution;
use serde::{Deserialize, Serialize};

/// What the ledger remembers about a shim dots created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedShim {
    pub package: String,
    /// The path it pointed at when created.
    pub target: String,
    pub run: String,
    pub ts: String,
}

/// A shim a package's chosen provider declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimDecl {
    pub package: String,
    /// The shim's name (`fd`).
    pub name: String,
    /// The executable it points at (`fdfind`).
    pub bin: String,
    /// `<home>/.local/bin/<name>`.
    pub path: PathBuf,
}

/// One planned shim change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimStep {
    pub package: String,
    pub name: String,
    pub bin: String,
    /// Where the link is, as the user reads it (`~/.local/bin/fd`).
    pub dst: String,
    pub path: PathBuf,
    /// `create`, `update` (a dangling link) or `remove`.
    pub action: String,
    /// The target when it is known at plan time (the package may not be installed yet).
    pub target: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimStepResult {
    pub step: ShimStep,
    pub ok: bool,
    pub detail: Option<String>,
    pub dur_ms: u64,
}

/// The state of a shim's link on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimState {
    /// A symlink whose target exists.
    Ok,
    Missing,
    /// A symlink whose target is gone.
    Dangling,
    /// A real file or directory: not ours to touch.
    Occupied,
}

pub fn bin_dir(home: &Path) -> PathBuf {
    home.join(".local/bin")
}

/// `~/...` for display.
pub fn tilde(home: &Path, p: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rel) => format!("~/{}", dots_core::paths::slashed(rel)),
        Err(_) => p.display().to_string(),
    }
}

/// How the link at `path` stands.
pub fn state_of(path: &Path) -> ShimState {
    match std::fs::symlink_metadata(path) {
        Err(_) => ShimState::Missing,
        Ok(m) if m.file_type().is_symlink() => {
            if path.exists() {
                ShimState::Ok
            } else {
                ShimState::Dangling
            }
        }
        Ok(_) => ShimState::Occupied,
    }
}

/// The shim declared by `package`'s chosen provider, if any.
pub fn declared_for(
    repo: &Repo,
    resolution: &Resolution,
    package: &str,
    home: &Path,
) -> Option<ShimDecl> {
    let pr = resolution.get(package).filter(|p| p.is_active())?;
    let choice = pr.chosen_provider()?;
    let pkg = repo.packages.get(package)?;
    let common = pkg.providers.get(&choice.id)?.common();
    let name = common.shim?;
    let bin = common
        .bin
        .or_else(|| common.bins.first().cloned())
        .or_else(|| pkg.bin.clone())
        .or_else(|| pkg.bins.first().cloned())?;
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    Some(ShimDecl {
        package: package.to_string(),
        path: bin_dir(home).join(&name),
        name,
        bin,
    })
}

/// Every shim of every active package.
pub fn declared(repo: &Repo, resolution: &Resolution, home: &Path) -> Vec<ShimDecl> {
    resolution
        .order
        .iter()
        .filter_map(|n| declared_for(repo, resolution, n, home))
        .collect()
}

/// Find the executable `bin` on `path_var` (a `PATH` value) and in the usual system dirs,
/// skipping `skip_dir` (the shim directory itself, so a shim never points at another shim).
pub fn find_bin(bin: &str, path_var: &str, home: &Path, skip_dir: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path_var).collect();
    #[cfg(unix)]
    for d in [
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/opt/homebrew/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs.push(home.join(".cargo/bin"));
    dirs.into_iter()
        .filter(|d| d.is_absolute() && d != skip_dir)
        .find_map(|d| dots_core::exe::find_in(&d, bin))
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Point `link` at `target`: written next to it and renamed over, so a reader never sees a
/// missing link. A real file or directory at `link` is refused.
pub fn make_link(link: &Path, target: &Path) -> Result<(), String> {
    if state_of(link) == ShimState::Occupied {
        return Err(format!(
            "{} exists and is not a symlink; dots does not replace it",
            link.display()
        ));
    }
    let dir = link.parent().ok_or("shim has no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".dots-shim-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    symlink(target, &tmp).map_err(|e| format!("cannot link {}: {e}", link.display()))?;
    std::fs::rename(&tmp, link).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot link {}: {e}", link.display())
    })
}

/// What [`plan`] reads.
pub struct ShimInputs<'a> {
    pub repo: &'a Repo,
    pub resolution: &'a Resolution,
    /// The active packages in scope.
    pub packages: &'a [String],
    /// Packages with an install step in this run (their software is not there yet).
    pub installing: &'a BTreeSet<String>,
    /// Packages the providers already report as installed.
    pub installed: &'a BTreeSet<String>,
    pub ledger: &'a BTreeMap<String, OwnedShim>,
    /// Full scope: also remove the ledger's shims that no active package declares.
    pub full: bool,
    pub home: &'a Path,
    /// The `PATH` value to look for the target in.
    pub path_var: &'a str,
}

/// The shim steps for this run: create or repair the shims of the packages in scope, and, for
/// a full-scope run, remove the ledger's shims that no active package declares any more.
pub fn plan(i: &ShimInputs<'_>) -> Vec<ShimStep> {
    let home = i.home;
    let mut out = Vec::new();
    for pkg in i.packages {
        let Some(d) = declared_for(i.repo, i.resolution, pkg, home) else {
            continue;
        };
        if !i.installing.contains(pkg) && !i.installed.contains(pkg) {
            continue;
        }
        let action = match state_of(&d.path) {
            ShimState::Ok => continue,
            ShimState::Dangling => "update",
            ShimState::Missing | ShimState::Occupied => "create",
        };
        out.push(ShimStep {
            package: d.package.clone(),
            dst: tilde(home, &d.path),
            target: find_bin(&d.bin, i.path_var, home, &bin_dir(home)),
            name: d.name,
            bin: d.bin,
            path: d.path,
            action: action.into(),
        });
    }
    if i.full {
        let wanted: Vec<PathBuf> = declared(i.repo, i.resolution, home)
            .into_iter()
            .map(|d| d.path)
            .collect();
        for (path, owned) in i.ledger {
            let p = PathBuf::from(path);
            // Gone already, or replaced by something that is not ours.
            if wanted.contains(&p)
                || matches!(state_of(&p), ShimState::Missing | ShimState::Occupied)
            {
                continue;
            }
            out.push(ShimStep {
                package: owned.package.clone(),
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                bin: String::new(),
                dst: tilde(home, &p),
                path: p,
                action: "remove".into(),
                target: Some(PathBuf::from(&owned.target)),
            });
        }
    }
    out
}
