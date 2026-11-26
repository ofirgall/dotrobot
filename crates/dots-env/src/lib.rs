//! Environment contributions (`[env]`): collection, per-shell generation, the single rc inject
//! and in-run propagation.
//!
//! Ordering rule: contributions are given in resolution order (dependencies first). A later
//! contribution is *prepended later*, so its PATH entries come earlier in the final PATH and its
//! variables override earlier ones. Within one package, `path` keeps its declared order.

mod collect;
mod inject;
mod shells;

pub use collect::{Contribution, EnvCtx, EnvOverlay};
pub use inject::{
    InjectOutcome, MARKER_CLOSE, MARKER_OPEN, default_rc, inject_rc, rc_line, remove_rc_inject,
};
pub use shells::{EnvFiles, Shell, generate};

use std::io;
use std::path::PathBuf;

use dots_core::paths::Dirs;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("invalid environment variable name `{0}`")]
    BadVarName(String),
    #[error("{} has `{MARKER_OPEN}` without `{MARKER_CLOSE}`; fix it by hand", .0.display())]
    UnterminatedBlock(PathBuf),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Generate and atomically write `env.sh`, `env.fish` and `env.ps1` into the state dir.
/// Files whose content is unchanged are not rewritten. Returns the paths written.
pub fn write_env_files(dirs: &Dirs, files: &EnvFiles) -> Result<Vec<PathBuf>, EnvError> {
    std::fs::create_dir_all(&dirs.state)?;
    let mut written = Vec::new();
    for sh in Shell::ALL {
        let path = dirs.env_file(sh.ext());
        let content = files.get(sh);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(content) {
            continue;
        }
        dots_files::atomic_write(&path, content.as_bytes())?;
        written.push(path);
    }
    Ok(written)
}

/// Full sync: generate from `contribs`, write the three files, and ensure the single rc inject
/// for `shell` in `rc`. Does nothing to `rc` when `rc` is `None`.
pub fn sync(
    dirs: &Dirs,
    ctx: &EnvCtx,
    contribs: &[Contribution],
    shell: Shell,
    rc: Option<&std::path::Path>,
) -> Result<InjectOutcome, EnvError> {
    let files = generate(contribs, ctx)?;
    write_env_files(dirs, &files)?;
    match rc {
        Some(rc) => inject_rc(rc, shell, &dirs.env_file(shell.ext()), &dirs.home),
        None => Ok(InjectOutcome::Unchanged),
    }
}

/// When no package contributes any `[env]`: write the three env files empty (regenerated from
/// no contributions) and remove the rc inject block. Text outside the markers is untouched.
pub fn clear(
    dirs: &Dirs,
    ctx: &EnvCtx,
    rc: Option<&std::path::Path>,
) -> Result<InjectOutcome, EnvError> {
    write_env_files(dirs, &generate(&[], ctx)?)?;
    match rc {
        Some(rc) => remove_rc_inject(rc),
        None => Ok(InjectOutcome::Unchanged),
    }
}
