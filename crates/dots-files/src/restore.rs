//! Backups of replaced paths and `dots restore`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use dots_core::report::Diagnostic;
use serde::{Deserialize, Serialize};

use crate::fsops::{atomic_write, backup_location, lexists, move_path, remove_any, sha256_path};
use crate::ledger::Ledger;
use crate::model::BackupKind;

/// One backed-up path, with what dots wrote in its place (to detect later changes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub dst: PathBuf,
    /// Where the original is kept.
    pub backup: PathBuf,
    pub expect_link: Option<PathBuf>,
    pub expect_sha: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub run_id: String,
    pub entries: Vec<BackupEntry>,
}

fn manifest_path(state: &Path, run_id: &str) -> PathBuf {
    state.join("backup").join(run_id).join("manifest.json")
}

fn load(state: &Path, run_id: &str) -> Option<Manifest> {
    serde_json::from_slice(&fs::read(manifest_path(state, run_id)).ok()?).ok()
}

fn save(state: &Path, m: &Manifest) -> io::Result<()> {
    let p = manifest_path(state, &m.run_id);
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    atomic_write(
        &p,
        serde_json::to_string_pretty(m)
            .unwrap_or_default()
            .as_bytes(),
    )
}

/// Back up the path at `dst` before it is replaced. Returns where the backup lives.
pub fn back_up(
    state: &Path,
    run_id: &str,
    dst: &Path,
    kind: BackupKind,
    expect_link: Option<&Path>,
    expect_sha: Option<&str>,
) -> io::Result<PathBuf> {
    let loc = backup_location(&state.join("backup").join(run_id), dst);
    let mut m = load(state, run_id).unwrap_or(Manifest {
        run_id: run_id.to_string(),
        entries: vec![],
    });
    // Keep the first backup of a path within a run: it is the user's original.
    if m.entries.iter().any(|e| e.dst == dst) {
        return Ok(loc);
    }
    if let Some(d) = loc.parent() {
        fs::create_dir_all(d)?;
    }
    match kind {
        BackupKind::Move => move_path(dst, &loc)?,
        BackupKind::Copy => {
            fs::copy(dst, &loc)?;
        }
        BackupKind::None => return Ok(loc),
    }
    m.entries.push(BackupEntry {
        dst: dst.to_path_buf(),
        backup: loc.clone(),
        expect_link: expect_link.map(Path::to_path_buf),
        expect_sha: expect_sha.map(str::to_string),
    });
    save(state, &m)?;
    Ok(loc)
}

#[derive(Debug, Default)]
pub struct RestoreReport {
    pub restored: Vec<PathBuf>,
    /// `restore-skipped` (changed since) and write failures.
    pub skipped: Vec<Diagnostic>,
}

/// Run ids that have backups, oldest first.
pub fn list_runs(state: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(state.join("backup"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().join("manifest.json").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Put back what `run_id` backed up. A destination that changed after dots wrote it is left
/// alone and reported as `restore-skipped`.
pub fn restore(state: &Path, run_id: &str, dry_run: bool) -> Result<RestoreReport, Diagnostic> {
    let Some(mut m) = load(state, run_id) else {
        return Err(Diagnostic::from_code(
            "backup-not-found",
            &[("run_id", run_id)],
        ));
    };
    let mut report = RestoreReport::default();
    let mut ledger = Ledger::load(state);
    let mut remaining = Vec::new();
    for e in std::mem::take(&mut m.entries) {
        let unchanged = if !lexists(&e.dst) {
            true
        } else if let Some(t) = &e.expect_link {
            fs::read_link(&e.dst).ok().as_ref() == Some(t)
        } else if let Some(h) = &e.expect_sha {
            sha256_path(&e.dst).as_ref() == Some(h)
        } else {
            false
        };
        if !unchanged || !lexists(&e.backup) {
            report.skipped.push(Diagnostic::from_code(
                "restore-skipped",
                &[("path", &e.dst.display().to_string()), ("run_id", run_id)],
            ));
            remaining.push(e);
            continue;
        }
        if !dry_run {
            let res = remove_any(&e.dst).and_then(|()| {
                if let Some(d) = e.dst.parent() {
                    fs::create_dir_all(d)?;
                }
                move_path(&e.backup, &e.dst)
            });
            if let Err(err) = res {
                report.skipped.push(Diagnostic::from_code(
                    "write-failed",
                    &[
                        ("path", &e.dst.display().to_string()),
                        ("detail", &err.to_string()),
                    ],
                ));
                remaining.push(e);
                continue;
            }
            ledger.owned.retain(|_, r| r.dst != e.dst);
        }
        report.restored.push(e.dst);
    }
    if !dry_run {
        m.entries = remaining;
        if m.entries.is_empty() {
            let _ = fs::remove_file(manifest_path(state, run_id));
        } else {
            save(state, &m).map_err(|e| {
                Diagnostic::from_code(
                    "write-failed",
                    &[("path", run_id), ("detail", &e.to_string())],
                )
            })?;
        }
        ledger.save(state).map_err(|e| {
            Diagnostic::from_code(
                "write-failed",
                &[("path", "files.json"), ("detail", &e.to_string())],
            )
        })?;
    }
    Ok(report)
}
